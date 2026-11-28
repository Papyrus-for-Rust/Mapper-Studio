use super::*;
use egui::AtomExt as _;
use egui::containers::menu::{MenuButton, MenuConfig};
use mapper_studio::generation_plan::{
    BatchPlan, DOMAINS, MAX_BATCH, NO_SEED_VARIATION, available_seed_domains, has_varying_seeds,
};
use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::hash::{BuildHasher, RandomState};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};
pub(super) mod persistence;
mod monument_edits;
mod queue_execution;
mod queue_table;
mod seed_scene;
pub(super) use seed_scene::Scene as SeedScene;
pub(super) use monument_edits::MonumentTemplate;
use monument_edits::MonumentChanges;

const RESIDENT_PREVIEWS: usize = 3;
const MAP_UNDO_LIMIT: usize = 128;
const MONUMENT_PICK_RADIUS: f32 = 18.;

struct PreviewLoad {
    id: u64,
    overview: Arc<tempfile::TempPath>,
    cancel: std::sync::Arc<AtomicBool>,
    receiver: std::sync::mpsc::Receiver<Result<Option<egui::ColorImage>, String>>,
}

impl Drop for PreviewLoad {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

struct Run {
    active_id: u64,
    template: Option<Project>,
    start: usize,
    finished: usize,
    terrain_ready: bool,
}
impl Run {
    fn remove_earlier_item(&mut self, index: usize) {
        if index >= self.finished { return; }
        self.finished -= 1;
        self.start -= usize::from(index < self.start);
    }
}
#[derive(Clone, Copy)]
struct MonumentDrag {
    index: usize,
    press: Pos2,
    origin: [f32; 3],
    previous: Option<[f32; 3]>,
}

#[derive(Clone)]
enum DraftChange {
    Monument {
        index: usize,
        before: Option<[f32; 3]>,
        after: Option<[f32; 3]>,
    },
    Spline(mapper_studio::path_edit::Change),
    Roster { before: Arc<MonumentChanges>, after: Arc<MonumentChanges> },
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EditIssue {
    id: u64,
    incomplete: bool,
    detail: String,
}

impl EditIssue {
    fn from_item(item: &QueueItem) -> Option<Self> {
        matches!(item.state, State::Failed | State::Incomplete).then(|| Self {
            id: item.id,
            incomplete: item.state == State::Incomplete,
            detail: item.detail.clone(),
        })
    }
}

#[derive(Default)]
struct MapEdit {
    selected_monument: Option<usize>,
    poses: std::collections::BTreeMap<usize, [f32; 3]>,
    move_limit_m: Option<u32>,
    monuments: Arc<MonumentChanges>,
    press: Option<MonumentDrag>,
    drag: Option<MonumentDrag>,
    job: Option<u64>,
    last_issue: Option<EditIssue>,
    apply_error: Option<String>,
    paths: path_editor::Draft,
    canvas: Option<egui::Id>,
    undo: VecDeque<DraftChange>,
    redo: Vec<DraftChange>,
}

impl MapEdit {
    fn record(&mut self, change: DraftChange) {
        self.apply_error = None;
        if self.undo.len() == MAP_UNDO_LIMIT {
            self.undo.pop_front();
        }
        self.undo.push_back(change);
        self.redo.clear();
    }

    fn can_step(&self, redo: bool) -> bool {
        self.job.is_none() && self.press.is_none() && !self.dragging()
            && if redo { !self.redo.is_empty() } else { !self.undo.is_empty() }
    }

    fn step(&mut self, redo: bool, result: &Candidate) -> Result<bool, String> {
        if !self.can_step(redo) {
            return Ok(false);
        }
        let change = if redo { self.redo.last() } else { self.undo.back() };
        match change.expect("A history step is available").clone() {
            DraftChange::Monument { index, before, after } => {
                if let Some(pose) = if redo { after } else { before } {
                    self.poses.insert(index, pose);
                } else {
                    self.poses.remove(&index);
                }
            }
            DraftChange::Spline(change) => {
                self.paths.step_history(&result.routes, change, result.recipe.size, redo)?;
            }
            DraftChange::Roster { before, after } => {
                self.monuments = if redo { after } else { before };
            }
        }
        if redo {
            self.undo.push_back(self.redo.pop().unwrap());
        } else {
            self.redo.push(self.undo.pop_back().unwrap());
        }
        self.apply_error = None;
        Ok(true)
    }

    fn staged_poses(&self) -> impl Iterator<Item = (usize, [f32; 3])> + '_ {
        let drag = self.drag;
        self.poses.iter().map(|(&index, &pose)| (index, pose))
            .filter(move |(index, _)| drag.is_none_or(|drag| drag.index != *index))
            .chain(drag.and_then(|drag| drag.previous.map(|pose| (drag.index, pose))))
    }

    fn has_changes(&self) -> bool {
        !self.poses.is_empty() || !self.paths.changes().is_empty() || !self.monuments.is_empty()
    }

    fn dragging(&self) -> bool {
        self.drag.is_some() || self.paths.dragging()
    }

    fn protects_result(&self) -> bool {
        self.dragging() || self.has_changes()
    }

    fn move_limit(&self, size: u32) -> u32 {
        self.move_limit_m.unwrap_or(mapper_studio::DEFAULT_MONUMENT_MOVE_LIMIT_M).min(size)
    }

    fn recipe(&self, original: &Project, monuments: &[map_monuments::Monument]) -> Result<Project, String> {
        let mut recipe = original.clone();
        let mut moved_initial = false;
        let move_limit = self.move_limit(original.size);
        for (&index, &[x, z, yaw]) in &self.poses {
            let monument = monuments.get(index).ok_or("The monument to move is unavailable")?;
            if let Some(index) = target_index(original, monument) {
                let target = &mut recipe.targets[index];
                target.x = x;
                target.z = z;
                target.yaw = Some(yaw);
                target.max_displacement_m = Some(move_limit);
                moved_initial = true;
            } else if let Some(prefab) = roadside_prefab(original, monument) {
                let policy = recipe.roads.as_mut().and_then(|roads| roads.staged.as_mut())
                    .ok_or("Roadside edits require staged road generation")?;
                let preference = mapper_studio::road_generation::RoadsidePreference {
                    request: monument.id,
                    prefab: prefab.into(),
                    position: [x, z],
                    yaw,
                    max_displacement_m: Some(move_limit),
                    presence: policy.roadside_preferences.iter()
                        .find(|saved| saved.request == monument.id).and_then(|saved| saved.presence),
                };
                if let Some(existing) = policy.roadside_preferences.iter_mut()
                    .find(|saved| saved.request == monument.id)
                {
                    *existing = preference;
                } else {
                    policy.roadside_preferences.push(preference);
                    policy.roadside_preferences.sort_unstable_by_key(|saved| saved.request);
                }
            } else {
                return Err("The monument does not have an editable placement preference".into());
            }
        }
        if moved_initial && !mapper_studio::placement_preview::is_earthwork(&recipe.placement_engine) {
            recipe.placement_engine = mapper_studio::placement_preview::ROTATION_VERSION.into();
        }
        self.monuments.apply(&mut recipe, monuments)?;
        recipe.path_edits.extend_from_slice(self.paths.changes());
        mapper_studio::path_edit::validate(&recipe.path_edits, recipe.size)?;
        Ok(recipe)
    }

    fn cancel_drag(&mut self) {
        self.press = None;
        if let Some(drag) = self.drag.take() {
            if let Some(pose) = drag.previous {
                self.poses.insert(drag.index, pose);
            } else {
                self.poses.remove(&drag.index);
            }
        }
    }
}

struct Candidate {
    identity:Option<persistence::Identity>,
    overview: Option<std::sync::Arc<tempfile::TempPath>>,
    archive: Option<std::sync::Arc<map_archive::Archive>>,
    archive_issue: String,
    id: u64,
    label: String,
    recipe: Project,
    image: Option<Result<egui::TextureHandle, String>>,
    zoom: f32,
    pan: Vec2,
    placed: usize,
    missing: usize,
    monuments: Vec<map_monuments::Monument>,
    routes: Vec<mapper_studio::path_edit::Route>,
    restrictions: restriction_panel::Panel,
    grid: bool,
    no_build: bool,
    map_font: Option<std::sync::Arc<mapper_studio::map_labels::Prepared>>,
    background: Color32,
    palette: Option<std::sync::Arc<mapper_studio::terrain_palette::Palette>>,
}
impl Candidate {
    fn shares_overview(&self, overview: &Arc<tempfile::TempPath>) -> bool {
        self.overview.as_ref().is_some_and(|file| Arc::ptr_eq(file, overview))
    }

    fn fit_map(&mut self) {
        self.zoom = 1.;
        self.pan = Vec2::ZERO;
    }

    fn exportable_archive(&self) -> Option<&std::sync::Arc<map_archive::Archive>> {
        if self.missing > 0 || !self.archive_issue.is_empty() {
            return None;
        }
        self.archive.as_ref()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum State {
    Waiting,
    Running,
    Ready,
    Incomplete,
    Failed,
    Cancelled,
}
impl State {
    fn retains_result(self) -> bool {
        matches!(self, Self::Ready | Self::Incomplete)
    }
}
fn completion_issue(recipe: &Project, missing: usize, has_archive: bool, archive_issue: &str) -> Option<String> {
    if !archive_issue.is_empty() {
        return Some(archive_issue.into());
    }
    if missing > 0 {
        return Some(format!("{missing} required monuments could not be placed"));
    }
    if mapper_studio::native_preview::is_native(&recipe.preview_engine)
        && recipe.composition_engine.is_some()
        && !has_archive
    {
        return Some("Generated map archive is unavailable".into());
    }
    None
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct QueueItem {
    id: u64,
    label: String,
    recipe: Project,
    state: State,
    detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    origin: Option<recipe_tabs::RecipeOrigin>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Submission {
    Queue,
    Run,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QueueAction {
    OpenMap(u64),
    UseSeed(u32),
    LoadRecipe(u64),
    SaveRecipe(u64),
    CancelMap(u64),
    StartMap(u64),
    StartAll,
    TogglePause(u64),
    ToggleQueuePause,
    Requeue(u64),
    RunNext(u64),
    MoveWaiting { id: u64, target: u64 },
    StepWaiting { id: u64, direction: isize },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
enum QueueFilter {
    #[default]
    All,
    Active,
    Issues,
    Ready,
}

impl QueueFilter {
    const ALL: [Self; 4] = [Self::All, Self::Active, Self::Issues, Self::Ready];

    fn label(self) -> &'static str {
        match self {
            Self::All => "All jobs",
            Self::Active => "Active",
            Self::Issues => "Issues",
            Self::Ready => "Ready",
        }
    }

    fn includes(self, state: State) -> bool {
        match self {
            Self::All => true,
            Self::Active => matches!(state, State::Waiting | State::Running),
            Self::Issues => matches!(state, State::Incomplete | State::Failed),
            Self::Ready => state == State::Ready,
        }
    }
}

pub(super) struct Generation {
    pub(super) revision:u64,
    pub(super) recipe_recovery: Option<diagnostic_ui::RecipeSection>,
    overview:Option<persistence::OverviewTask>,
    overview_ready: Option<(persistence::OverviewKey, std::sync::Arc<tempfile::TempPath>)>,
    validated_recipe: Option<(Project, Result<(), GenerationBlocker>)>,
    other_validated_recipes: HashMap<u64, (Project, Result<(), GenerationBlocker>)>,
    count: usize,
    randomize: [bool; 7],
    run: Option<Run>,
    draft_target: Option<u64>,
    draft_snapshot: Option<Project>,
    results: Vec<Candidate>,
    road_layouts: HashMap<u64, Arc<mapper_studio::source_road_planning::AcceptedRoadLayout>>,
    edits: std::collections::HashMap<u64, MapEdit>,
    resident_previews: VecDeque<u64>,
    preview_load: Option<PreviewLoad>,
    cancelled_preview: Option<PreviewLoad>,
    queue: Vec<QueueItem>,
    queue_filter: QueueFilter,
    queue_paused: bool,
    execution: queue_execution::QueueExecution,
    revealed_job: Option<u64>,
    queue_focus: Option<u64>,
    next_id: u64,
    pending_tabs: Vec<u64>,
    retired_tabs: Vec<u64>,
    closed_job_tabs: std::collections::HashSet<u64>,
    started: Option<Instant>,
    elapsed: std::collections::HashMap<u64, Duration>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum GenerationBlocker {
    MissingSeed,
    MatchingInstallation,
    UnsupportedNativeProfile(String),
    AssetsUnavailable,
    QueueFull,
    CheckingOverlays,
    Overlay { anchor: u32, message: String },
    Other(String),
}

impl GenerationBlocker {
    pub(super) fn message(&self) -> &str {
        match self {
            Self::MissingSeed => mapper_studio::MASTER_SEED_REQUIRED,
            Self::MatchingInstallation => "Choose the matching Rust installation in Settings",
            Self::AssetsUnavailable => "Installation assets unavailable",
            Self::QueueFull => "The generation queue is full. Wait for a map to finish or remove one in Queue.",
            Self::CheckingOverlays => "Checking monument layouts…",
            Self::Overlay { message, .. }
            | Self::UnsupportedNativeProfile(message)
            | Self::Other(message) => message,
        }
    }
}

impl From<String> for GenerationBlocker {
    fn from(message: String) -> Self {
        Self::Other(message)
    }
}

impl Default for Generation {
    fn default() -> Self {
        Self {
            revision:0,overview:None,overview_ready:None,
            recipe_recovery: None,
            validated_recipe: None,
            other_validated_recipes: HashMap::new(),
            count: 4,
            randomize: [false; 7],
            run: None,
            draft_target: None,
            draft_snapshot: None,
            results: Vec::new(),
            road_layouts: HashMap::new(),
            edits: Default::default(),
            resident_previews: VecDeque::new(),
            preview_load: None,
            cancelled_preview: None,
            queue: Vec::new(),
            queue_filter: QueueFilter::All,
            queue_paused: false,
            execution: Default::default(),
            revealed_job: None,
            queue_focus: None,
            next_id: 1,
            pending_tabs: Vec::new(),
            retired_tabs: Vec::new(),
            closed_job_tabs: Default::default(),
            started: None,
            elapsed: Default::default(),
        }
    }
}
impl Generation {
    pub(super) fn draft(&self) -> Option<&Project> {
        self.run.as_ref().and_then(|run| run.template.as_ref())
    }

    pub(super) fn active_edit_road_layout(&self) -> Option<Arc<mapper_studio::source_road_planning::AcceptedRoadLayout>> {
        let active = self.run.as_ref()?.active_id;
        let (&original, _) = self.edits.iter().find(|(_, edit)| edit.job == Some(active))?;
        self.road_layouts.get(&original).cloned()
    }

    fn active_recipe(&self) -> Option<&Project> {
        let run = self.run.as_ref()?;
        self.queue.get(run.finished)
            .filter(|item| item.id == run.active_id)
            .map(|item| &item.recipe)
    }

    fn remember_preview(&mut self, id: u64) {
        if self.resident_previews.back() == Some(&id) {
            return;
        }
        self.resident_previews.retain(|&previous| previous != id);
        self.resident_previews.push_back(id);
    }

    fn reap_cancelled_preview(&mut self) {
        use std::sync::mpsc::TryRecvError;
        if self.cancelled_preview.as_ref().is_some_and(|load| {
            !matches!(load.receiver.try_recv(), Err(TryRecvError::Empty))
        }) {
            self.cancelled_preview = None;
        }
        // A cancelled read can be stuck in I/O. Give the visible preview
        // a slot while waiting for that read, with at most two workers.
        if self.cancelled_preview.is_none()
            && self.preview_load.as_ref().is_some_and(|load| load.cancel.load(Ordering::Relaxed))
        {
            self.cancelled_preview = self.preview_load.take();
        }
    }

    fn prepare_preview(&mut self, id: u64, ctx: &egui::Context) {
        if let Some(load) = &self.preview_load {
            use std::sync::mpsc::TryRecvError;
            let ready = match load.receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Disconnected) => Some(Err("Map overview worker stopped".into())),
                Err(TryRecvError::Empty) => None,
            };
            if let Some(ready) = ready {
                let load = self.preview_load.take().unwrap();
                let cancelled = load.cancel.load(Ordering::Relaxed);
                if !cancelled
                    && let Some(candidate) = self.results.iter_mut().find(|map| {
                        (map.id == load.id || map.id == id)
                            && map.image.is_none()
                            && map.shares_overview(&load.overview)
                    })
                {
                    let loaded_id = candidate.id;
                    match ready {
                        Ok(Some(image)) => {
                            candidate.image = Some(Ok(ctx.load_texture(
                                format!("generated-map-{loaded_id}"),
                                image,
                                egui::TextureOptions::LINEAR,
                            )));
                            self.remember_preview(loaded_id);
                        }
                        Err(error) => candidate.image = Some(Err(error)),
                        Ok(None) => {}
                    }
                }
            }
        }
        self.reap_cancelled_preview();
        let Some(index) = self.results.iter().position(|candidate| candidate.id == id) else {
            return;
        };
        let candidate = &self.results[index];
        if matches!(candidate.image, Some(Ok(_))) {
            self.remember_preview(id);
            return;
        }
        let Some(overview) = candidate.overview.as_ref().filter(|_| candidate.image.is_none()) else {
            return;
        };
        // An accepted overview is immutable. The tab draws its own overlays
        // above the texture, so a shared payload can also share its pixels.
        if let Some(image) = self.results.iter()
            .filter(|map| map.shares_overview(overview))
            .find_map(|map| map.image.as_ref().and_then(|image| image.as_ref().ok()))
            .cloned()
        {
            self.results[index].image = Some(Ok(image));
            self.remember_preview(id);
            return;
        }
        if self.preview_load.is_some() {
            return;
        }
        let overview = overview.clone();
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let signal = cancel.clone();
        let source = overview.clone();
        let receiver = preview::start_worker("mapper-overview", ctx.clone(), move |_| {
            persistence::read_overview(source.as_ref(), &|| {
                signal.load(Ordering::Relaxed)
            })
        });
        self.preview_load = Some(PreviewLoad {
            id,
            overview,
            cancel,
            receiver,
        });
    }

    pub(super) fn finish_preview_frame(&mut self, dock: &DockState<Tab>) {
        let visible = |id| dock.find_tab(&Tab::Generated(id)).is_some_and(|path| {
            dock.leaf(path.node_path()).is_ok_and(|leaf| leaf.active == path.tab)
        });
        // Active leaves can contain more maps than the normal preview budget.
        // Wait until the dock is settled and only evict hidden textures.
        while self.resident_previews.len() > RESIDENT_PREVIEWS {
            let Some(index) = self.resident_previews.iter().position(|&id| !visible(id)) else {
                break;
            };
            let old = self.resident_previews.remove(index).unwrap();
            if let Some(candidate) = self.results.iter_mut().find(|candidate| candidate.id == old) {
                candidate.image = None;
            }
        }
        if let Some(load) = &mut self.preview_load
            && !load.cancel.load(Ordering::Relaxed)
        {
            // The dock can activate a different tab after its widgets finish drawing.
            if let Some(map) = self.results.iter().find(|map| {
                map.image.is_none() && map.shares_overview(&load.overview) && visible(map.id)
            }) {
                load.id = map.id;
            } else {
                load.cancel.store(true, Ordering::Relaxed);
            }
        }
        self.reap_cancelled_preview();
        for (&id, edit) in &mut self.edits {
            if (edit.press.is_some() || edit.dragging()) && !visible(id) {
                edit.cancel_drag();
                edit.paths.cancel_gesture();
            }
        }
    }

    pub(super) fn sync_restrictions(&mut self, source: &restriction_panel::Panel) {
        for result in &mut self.results {
            result.restrictions.sync(&result.recipe, source, &mut result.monuments);
        }
    }

    pub(super) fn sync_map_font(&mut self, source: &Arc<mapper_studio::map_labels::Prepared>) {
        for result in &mut self.results {
            if result.recipe.installation.as_ref() != Some(&source.installation) {
                continue;
            }
            let compatible = result.map_font.as_ref().is_none_or(|font| {
                font.sha256 == source.sha256
                    && source.grid.as_ref().is_some_and(|grid| {
                        font.grid.as_ref().is_none_or(|older| grid.adds_line_samples_to(older))
                    })
            });
            if compatible {
                result.map_font = Some(source.clone());
            }
        }
    }
    pub(super) fn open_maps(&self,dock:&DockState<Tab>)->Vec<u64> {
        dock.iter_all_tabs().filter_map(|(_,tab)|match tab {
            Tab::Generated(id) if self.results.iter().any(|result|result.id==*id)=>Some(*id),
            _=>None,
        }).collect()
    }
    pub fn busy(&self) -> bool {
        self.run.is_some()
    }
    pub(super) fn primary_budget(&self) -> Option<mapper_studio::generation_resources::JobBudget> {
        self.execution.primary_budget.clone()
    }
    pub(super) fn primary_waiting(&self) -> bool {
        self.execution.start_pending.is_some() || self.execution.advance_pending
    }
    pub(super) fn replace_preserving_worker_budgets(&mut self, replacement: Self) {
        let retiring = std::mem::take(&mut self.execution.retiring);
        *self = replacement;
        self.execution.retiring = retiring;
    }
    fn available_slots(&self) -> usize {
        let first_running = self.queue.iter().position(|item| matches!(item.state, State::Running | State::Waiting));
        let protected = match (first_running, self.run.as_ref()) {
            (Some(first), Some(run)) => first.min(run.finished),
            (Some(first), None) => first,
            (None, Some(run)) => run.finished,
            (None, None) => self.queue.len(),
        };
        MAX_BATCH.saturating_sub(self.queue.len() - protected)
    }
    fn history_len(&self) -> usize {
        let end = self.run.as_ref().map_or(self.queue.len(), |run| run.start);
        self.queue[..end].iter().position(|item| matches!(item.state, State::Running | State::Waiting)).unwrap_or(end)
    }
    fn discard_queue_prefix(&mut self, count: usize) {
        if count == 0 {
            return;
        }
        for item in self.queue.drain(..count) {
            self.closed_job_tabs.remove(&item.id);
            if !self.results.iter().any(|map| map.id == item.id) {
                self.elapsed.remove(&item.id);
                self.retired_tabs.push(item.id);
            }
        }
        self.revealed_job = None;
        self.queue_focus = None;
        if let Some(run) = &mut self.run {
            run.finished -= count;
            run.start = run.start.saturating_sub(count);
        }
    }
    fn can_enqueue(&self, count: usize) -> bool {
        count <= self.available_slots()
    }
    fn waiting_count(&self) -> usize {
        self.queue.iter().filter(|item| item.state == State::Waiting).count()
    }
    fn retire_waiting_map(&mut self, id: u64) {
        self.execution.requested_parallel.remove(&id);
        if self.execution.next_primary == Some(id) {
            self.execution.next_primary = None;
        }
        if self.queue_focus == Some(id) {
            self.queue_focus = None;
        }
        self.retired_tabs.push(id);
        self.closed_job_tabs.remove(&id);
        if let Some(edit) = self.edits.values_mut().find(|edit| edit.job == Some(id)) {
            edit.job = None;
        }
    }
    fn remove_waiting_maps(&mut self) {
        let mut removed = false;
        for index in (0..self.queue.len()).rev() {
            if self.queue[index].state == State::Waiting {
                let item = self.queue.remove(index);
                self.retire_waiting_map(item.id);
                removed = true;
            }
        }
        if removed { self.revision += 1; }
    }
    pub(super) fn result_monuments(&self, id:u64) -> Option<&[map_monuments::Monument]> {
        self.results.iter().find(|r|r.id==id).map(|r|r.monuments.as_slice())
    }

    pub(super) fn selected_monument(&self, id: u64) -> Option<usize> {
        self.edits.get(&id)?.selected_monument
    }

    pub(super) fn select_monument(&mut self, id: u64, index: usize) {
        if self.result_monuments(id).is_some_and(|monuments| index < monuments.len()) {
            self.edits.entry(id).or_default().selected_monument = Some(index);
        }
    }
}

impl Studio {
    fn remove_generated_tab(&mut self, id: u64) {
        let tab = Tab::Generated(id);
        let Some(path) = self.dock.find_tab(&tab) else { return; };
        let has_other_map = self.dock.iter_all_tabs()
            .any(|(_, item)| matches!(item, Tab::Home | Tab::Generated(_)) && *item != tab);
        if has_other_map {
            self.dock.remove_tab(path);
        } else if let Ok(leaf) = self.dock.leaf_mut(path.node_path()) {
            leaf.tabs[path.tab.0] = Tab::Home;
        }
    }

    pub(super) fn finish_map_tab_requests(&mut self) {
        for id in std::mem::take(&mut self.generation.retired_tabs) {
            self.remove_generated_tab(id);
        }
        let tabs = std::mem::take(&mut self.generation.pending_tabs);
        let anchor = tabs.first().and_then(|&id| self.dock.find_tab(&Tab::Generated(id)))
            .or_else(|| self.map_tab_path());
        let Some(path) = anchor else { return; };
        for id in tabs {
            let tab = Tab::Generated(id);
            if self.dock.find_tab(&tab).is_none()
                && self.generation.queue.iter().any(|job| job.id == id)
                && let Ok(leaf) = self.dock.leaf_mut(path.node_path())
            {
                leaf.tabs.push(tab);
            }
        }
    }

    pub(super) fn close_generation_tab(&mut self, id: u64) {
        if self.generation.queue.iter().any(|item| {
            item.id == id && matches!(item.state, State::Waiting | State::Running)
        }) {
            self.generation.closed_job_tabs.insert(id);
        }
    }

    fn record_generation_elapsed(&mut self) {
        if let Some(start) = self.generation.started.take()
            && let Some(run) = &self.generation.run
        {
            self.generation.elapsed.insert(run.active_id, self.generation.execution.frozen_primary.take().unwrap_or_else(Instant::now).saturating_duration_since(start));
        }
    }

    pub(super) fn live_job_tab_visible(&self) -> bool {
        self.live_generation && self.generation.run.as_ref().is_some_and(|run| {
            self.dock.find_tab(&Tab::Generated(run.active_id)).is_some_and(|path| {
                self.dock.leaf(path.node_path()).is_ok_and(|leaf| leaf.active == path.tab)
            })
        })
    }

    fn pending_map(&mut self, ui: &mut egui::Ui, id: u64) {
        let Some(index) = self.generation.queue.iter().position(|item| item.id == id) else {
            ui.label("This map is no longer in the workspace.");
            return;
        };
        let state = self.generation.queue[index].state;
        let active = state == State::Running && self.generation.run.as_ref()
            .is_some_and(|run| run.active_id == id);
        let extra_active = self.generation.execution.jobs.contains_key(&id);
        let elapsed = self.queue_elapsed(id);
        let (stage, fraction) = if state == State::Running && self.queue_job_paused(id) {
            (self.queue_pause_caption(id).into(), None)
        } else if active {
            self.queue_wait_caption(id).map_or_else(|| self.generation_stage(), |reason| (reason.into(), None))
        } else if state == State::Running {
            self.extra_queue_stage(id)
        } else {
            let stage = match state {
                State::Waiting => self.queue_wait_caption(id).unwrap_or("Queued"),
                State::Running => "Generating",
                State::Failed => "Generation failed",
                State::Cancelled => "Cancelled",
                State::Ready | State::Incomplete => "Map unavailable",
            };
            (stage.into(), None)
        };
        let row = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
        ui.allocate_ui_with_layout(row, egui::Layout::right_to_left(egui::Align::Center), |ui| {
            map_seed_menu(ui, &self.generation.queue[index].recipe, false);
            seed_scene::view_toggle(ui, &mut false, false);
            if (active || extra_active) && ui.checkbox(&mut self.live_generation, "Live")
                .on_hover_text("Show map previews as generation stages finish.")
                .changed() && !self.live_generation
            {
                self.clear_stage_preview();
            }
            if let Some(elapsed) = elapsed {
                ui.label(elapsed_time(elapsed)).on_hover_text("Elapsed generation time");
            }
            let contents = egui::vec2(ui.available_width().max(1.), row.y);
            ui.allocate_ui_with_layout(contents, egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add(egui::Label::new(stage.as_ref()).truncate())
                    .on_hover_text(stage.as_ref());
            });
        });
        if state == State::Waiting
            && let Some(run) = &self.generation.run
            && index > run.finished
        {
            let position = self.generation.queue[run.finished + 1..=index].iter()
                .filter(|item| item.state == State::Waiting).count();
            self.waiting_map(ui, id, run.active_id, position);
            return;
        }
        if state == State::Failed {
            let can_edit = self.can_load_queue_recipe();
            let can_retry = self.can_requeue();
            let detail = &self.generation.queue[index].detail;
            let summary = diagnostic_ui::summary(detail);
            let mut action = None;
            egui::ScrollArea::vertical().id_salt(("failed-map", id)).show(ui, |ui| {
                egui::Frame::NONE.inner_margin(12).show(ui, |ui| {
                    ui.set_max_width(ui.available_width().min(480.));
                    if !summary.is_empty() {
                        ui.add(egui::Label::new(summary.as_ref()).wrap());
                        ui.add_space(6.);
                    }
                    ui.horizontal_wrapped(|ui| {
                        if queue_retry_button(ui, "Retry", can_retry).clicked() {
                            action = Some(QueueAction::Requeue(id));
                        }
                        if queue_recipe_button(ui, can_edit).clicked() {
                            action = Some(QueueAction::LoadRecipe(id));
                        }
                        if ui.button("Save recipe…").clicked() {
                            action = Some(QueueAction::SaveRecipe(id));
                        }
                    });
                    if !detail.is_empty() {
                        ui.add_space(6.);
                        diagnostic_ui::copy_error(ui, detail, Some(&self.generation.queue[index].recipe));
                        ui.collapsing("Technical report", |ui| {
                            diagnostic_ui::copy_details(ui, detail);
                            if summary.as_ref() != detail {
                                diagnostic_ui::report_preview(ui, detail);
                            }
                        });
                    }
                });
            });
            self.apply_queue_action(action, ui.ctx());
            return;
        }
        if active {
            ui.ctx().request_repaint_after(Duration::from_secs(1));
            if self.live_generation {
                if let Some(image) = self.composition.take_live_image() {
                    self.stage_image = Some(image);
                }
                self.upload_stage_preview(ui.ctx());
            }
        }
        let (rect, _) = ui.allocate_exact_size(ui.available_size().max(Vec2::splat(1.)), Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0., if active { self.viewport_background() } else { MAP_BACKGROUND });
        let texture = (active && self.live_generation).then(|| {
            self.composition.view(&self.project).map(|view| &view.texture)
                .or(self.stage_texture.as_ref())
                .or_else(|| {
                    (self.preview_current() && self.task.is_none()).then_some(self.texture.as_ref()).flatten()
                })
        }).flatten().or_else(|| self.live_generation.then(|| self.extra_live_texture(id)).flatten());
        if let Some(texture) = texture {
            let size = rect.size().min_elem();
            painter.image(texture.id(), Rect::from_center_size(rect.center(), Vec2::splat(size)),
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1., 1.)), Color32::WHITE);
        }
        if let Some(fraction) = fraction.filter(|_| rect.height() >= 3.) {
            let track = Rect::from_min_size(rect.min, egui::vec2(rect.width(), 3.));
            ui.place(track, egui::ProgressBar::new(fraction)
                .desired_width(track.width())
                .desired_height(track.height())
                .corner_radius(0.)
                .fill(workbench_style::PRIMARY_ACTION))
                .on_hover_text(format!("{:.0}% of this phase", fraction * 100.));
        } else if active && self.live_generation && rect.height() >= 3. {
            let track = Rect::from_min_size(rect.min, egui::vec2(rect.width(), 2.));
            let accent = ui.visuals().hyperlink_color;
            painter.rect_filled(track, 0., accent.gamma_multiply(0.23));
            if !self.preferences.reduce_motion {
                let length = (track.width() * 0.18).min(84.);
                let phase = ui.input(|input| (input.time / 1.8).fract()) as f32;
                let x = track.left() - length + (track.width() + length) * phase;
                let pulse = Rect::from_min_size(egui::pos2(x, track.top()), egui::vec2(length, track.height()));
                let visible = pulse.intersect(track);
                if visible.is_positive() {
                    painter.rect_filled(visible, 0., accent.gamma_multiply(0.72));
                }
                ui.ctx().request_repaint_after(Duration::from_millis(50));
            }
        }
    }

    fn waiting_map(&mut self, ui: &mut egui::Ui, id: u64, running: u64, position: usize) {
        let (stage, _) = self.generation_stage();
        let canvas = ui.available_rect_before_wrap();
        ui.painter().rect_filled(canvas, 0., MAP_BACKGROUND);
        let mut action = None;
        ui.vertical_centered(|ui| {
            ui.add_space((canvas.height() * 0.32 - 26.).max(5.));
            egui::Frame::NONE
                .fill(ui.visuals().window_fill())
                .corner_radius(6.)
                .outer_margin(egui::Margin::symmetric(12, 0))
                .inner_margin(egui::Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ui.set_max_width(ui.available_width().min(330.));
                    ui.vertical_centered(|ui| {
                        if position == 1 {
                            ui.heading("Up next");
                        } else {
                            ui.heading(format!("{position} maps ahead"))
                                .on_hover_text("Includes the running map.");
                        }
                        ui.add(egui::Label::new(egui::RichText::new(stage.as_ref()).weak()).truncate())
                            .on_hover_text(stage.as_ref());
                        ui.add_space(4.);
                        if ui.button("Open running map").clicked() {
                            action = Some(QueueAction::OpenMap(running));
                        }
                        if position > 1 && run_next_button(ui).clicked() {
                            action = Some(QueueAction::RunNext(id));
                        }
                    });
                });
        });
        self.apply_queue_action(action, ui.ctx());
    }

    pub(super) fn generated_image_ready(&self, id: u64) -> bool {
        self.generation
            .results
            .iter()
            .find(|r| r.id == id)
            .is_some_and(|r| r.archive.as_ref().is_some_and(|a| a.image.is_some()))
    }
    pub(super) fn generated_image_issue(&self, id: u64) -> &str {
        self.generation.results.iter().find(|r|r.id==id).and_then(|r|r.archive.as_ref()).map_or("This preview has no retained terrain render. Turn off Terrain to export its overlays.",|a|a.image_issue.as_str())
    }
    pub(super) fn generated_image_recipe(&self, id: u64) -> Option<&Project> {
        self.generation
            .results
            .iter()
            .find(|r| r.id == id)
            .map(|r| &r.recipe)
    }
    pub(super) fn map_has_pending_edits(&self, id: u64) -> bool {
        self.generation.edits.get(&id).is_some_and(MapEdit::has_changes)
    }
    pub(super) fn generated_image_warnings(&self, id: u64) -> (usize, bool) {
        self.generation
            .results
            .iter()
            .find(|r| r.id == id)
            .map_or((0, false), |r| {
                (
                    r.restrictions.incomplete_count(&r.recipe, &r.monuments),
                    r.map_font.is_some(),
                )
            })
    }
    pub(super) fn generated_image_input(
        &self,
        id: u64,
    ) -> Option<(export_panel::Source, export_render::Scene)> {
        let result = self.generation.results.iter().find(|r| r.id == id)?;
        let source = result
            .archive
            .as_ref()
            .and_then(|a| a.image.clone())
            .map_or(export_panel::Source::Unavailable, |snapshot| {
                export_panel::Source::Saved {
                    snapshot,
                    palette: result.palette.clone(),
                }
            });
        Some((
            source,
            export_render::Scene {
                project: result.recipe.clone(),
                monuments: result.monuments.clone(),
                restrictions: result.restrictions.for_export(),
                icons: monument_icon_panel::Panel::with_export_catalog(
                    result.restrictions.catalog_for(&result.recipe),
                ),
                font: result.map_font.clone(),
                preferences: self.preferences.clone(),
                settings_path: self.settings_path.clone(),
                background: result.background,
            },
        ))
    }
    pub(super) fn focused_map_archive(&self)->Option<(std::sync::Arc<map_archive::Archive>,String,Option<map_archive::Notification>)> {
        let id=self.visible_generated_tab()?;
        let result=self.generation.results.iter().find(|r|r.id==id)?;
        Some((result.exportable_archive()?.clone(),
            result.recipe.options.filename(result.recipe.size, result.recipe.master_seed, &result.recipe.seeds, &result.label),
            map_archive::Notification::from_recipe(&result.recipe)))
    }
    fn validate_generation_recipe(recipe: &Project) -> Result<(), GenerationBlocker> {
        recipe.require_master_seed()?;
        recipe.validate()?;
        if mapper_studio::native_preview::is_native(&recipe.preview_engine) {
            let source = recipe.installation.as_ref().ok_or(GenerationBlocker::MatchingInstallation)?;
            mapper_studio::native_profile::for_source(source, recipe.native_profile.as_ref())
                .map_err(GenerationBlocker::UnsupportedNativeProfile)?;
        }
        Ok(())
    }

    fn generation_source_readiness(&self, recipe: &Project, refresh_overlays: bool) -> Result<(), GenerationBlocker> {
        if self.installation_library.resetting() {
            return Err(GenerationBlocker::Other("Resetting asset cache…".into()));
        }
        if mapper_studio::native_preview::is_native(&recipe.preview_engine) {
            if self.source.inspecting() {
                return Err(GenerationBlocker::Other("Checking Rust installation…".into()));
            }
            let Some(root) = self.source.bound_root_ref(recipe) else {
                return Err(GenerationBlocker::MatchingInstallation);
            };
            if !self.prefabs.generation_assets_ready(recipe, root) {
                return Err(if self.prefabs.loading_assets() {
                    GenerationBlocker::Other("Loading installation assets…".into())
                } else {
                    GenerationBlocker::AssetsUnavailable
                });
            }
            if self.installation_roster_pending {
                self.monuments.roster_readiness()?;
                return Err(GenerationBlocker::Other(
                    "Wait for the installation monument roster".into(),
                ));
            }
            self.prefabs.check_addons(recipe, root, refresh_overlays).map_err(|error| match error {
                super::prefab_panel::AddonReadinessError::Unavailable => GenerationBlocker::AssetsUnavailable,
                super::prefab_panel::AddonReadinessError::Pending => GenerationBlocker::CheckingOverlays,
                super::prefab_panel::AddonReadinessError::Invalid(issue) => match issue.anchor {
                    Some(anchor) => GenerationBlocker::Overlay { anchor, message: issue.message },
                    None => GenerationBlocker::Other(issue.message),
                },
            })?;
        }
        if !self.roads.ready(recipe) {
            return Err(GenerationBlocker::Other(
                "Wait for installation road data to finish preparing".into(),
            ));
        }
        Ok(())
    }

    fn generation_preflight(&self, recipe: &Project, working_recipe: bool) -> Result<(), String> {
        if working_recipe {
            self.decoration_panel.readiness()?;
            self.water.readiness(recipe)?;
        }
        Self::validate_generation_recipe(recipe).map_err(|issue| issue.message().to_owned())?;
        self.generation_source_readiness(recipe, true)
            .map_err(|issue| issue.message().to_owned())
    }

    pub(super) fn generation_readiness(&mut self) -> Result<(), GenerationBlocker> {
        self.generation_readiness_for(self.recipe_tabs.active())
    }

    fn generation_readiness_for(&mut self, id: u64) -> Result<(), GenerationBlocker> {
        if self.recipe_by_id(id).master_seed == 0 {
            return Err(GenerationBlocker::MissingSeed);
        }
        if self.drag_before.is_some() {
            return Err("Finish moving the recipe monument before generating.".to_owned().into());
        }
        if !self.generation.can_enqueue(1) {
            return Err(GenerationBlocker::QueueFull);
        }
        if let Some(project) = self.recipe_tabs.parked(id) {
            self.recipe_tabs.parked_readiness(id, project)?;
            let cached = self.generation.other_validated_recipes.entry(id)
                .or_insert_with(|| (project.clone(), Self::validate_generation_recipe(project)));
            if cached.0 != *project {
                *cached = (project.clone(), Self::validate_generation_recipe(project));
            }
            cached.1.clone()?;
            return self.generation_source_readiness(project, false);
        }
        let project = self.generation.run.as_ref().and_then(|run| run.template.as_ref()).unwrap_or(&self.project);
        self.decoration_panel.readiness()?;
        self.water.readiness(project)?;
        let cached = &mut self.generation.validated_recipe;
        if cached.as_ref().is_none_or(|(recipe, _)| recipe != project) {
            *cached = Some((project.clone(), Self::validate_generation_recipe(project)));
        }
        if let Some((_, Err(error))) = cached {
            return Err(error.clone());
        }
        self.generation_source_readiness(project, false)
    }
    #[cfg(test)]
    fn generate_recipe(&mut self, ctx: &egui::Context) {
        self.submit_active_recipe(Submission::Queue, ctx);
    }

    pub(super) fn submit_active_recipe(&mut self, submission: Submission, ctx: &egui::Context) {
        self.generate_recipe_for(self.recipe_tabs.active(), submission, ctx);
    }

    fn generate_recipe_for(&mut self, id: u64, submission: Submission, ctx: &egui::Context) {
        let recipe = self.recipe_by_id(id);
        if recipe.master_seed == 0 {
            self.status = mapper_studio::MASTER_SEED_REQUIRED.into();
            ctx.data_mut(|data| data.insert_temp(recipe_tabs::seed_id("missing-master-seed", id), true));
            ctx.memory_mut(|memory| memory.request_focus(recipe_tabs::seed_id("recipe-master-seed", id)));
            self.show_tab(Tab::Recipe(id));
            return;
        }
        self.submit_generation(id, BatchPlan::exact(recipe.clone()), submission, ctx);
    }

    pub(super) fn replace_recipe_draft(&mut self, recipe: Project) -> Project {
        let selected = recipe.targets.first().map(|target| target.instance_id);
        if let Some(draft) = self.generation.run.as_mut().and_then(|run| run.template.as_mut()) {
            self.generation.draft_snapshot = None;
            self.generation.draft_target = selected;
            return std::mem::replace(draft, recipe);
        }
        let source_changed = self.project.installation != recipe.installation;
        self.cancel_generation_work();
        self.plugins.cancel();
        self.topology_tool.cancel_stroke();
        self.composition = Default::default();
        self.layouts.clear();
        self.active_layout = None;
        self.selected = selected.map(|_| 0);
        self.restore_source |= source_changed;
        let old = std::mem::replace(&mut self.project, recipe);
        if !self.preview_current() {
            self.preview = None;
            self.texture = None;
            self.layer_texture = None;
            self.map_colors = Default::default();
            self.terrain_projection = Default::default();
        }
        old
    }

    fn use_queue_seed(&mut self, seed: u32) {
        if seed == 0 || seed > mapper_studio::MAX_SEED || self.drag_before.is_some() {
            return;
        }
        let busy = self.generation.run.as_ref().is_some_and(|run| run.template.is_some());
        self.with_recipe_draft(|studio| {
            if studio.project.master_seed != seed {
                let previous = (!busy).then(|| studio.project.clone());
                studio.project.master_seed = seed;
                studio.project.refresh_inherited_seeds();
                if let Some(previous) = previous {
                    studio.history.record(previous, &studio.project);
                    studio.skip_history = true;
                }
            }
        });
    }

    pub(super) fn randomize_master_seed(&mut self) {
        let mut seed = (RandomState::new().hash_one(()) % u64::from(mapper_studio::MAX_SEED)) as u32 + 1;
        if seed == self.project.master_seed {
            seed = seed % mapper_studio::MAX_SEED + 1;
        }
        self.project.master_seed = seed;
        self.project.refresh_inherited_seeds();
    }

    pub(super) fn document_recipe(&self) -> &Project {
        self.generation
            .run
            .as_ref()
            .and_then(|run| run.template.as_ref())
            .filter(|_| self.generation.active_recipe() == Some(&self.project))
            .unwrap_or(&self.project)
    }

    fn finish_generation_run(&mut self) {
        self.retire_primary_budget();
        let active_matches = self.generation.active_recipe() == Some(&self.project);
        let draft_target = self.generation.draft_target.take();
        self.generation.draft_snapshot = None;
        if let Some(template) = self.generation.run.take().and_then(|run| run.template)
            && active_matches
        {
            self.selected = draft_target.and_then(|id| {
                template.targets.iter().position(|target| target.instance_id == id)
            });
            if self.project == template {
                return;
            }
            self.restore_source |= self.project.installation != template.installation;
            self.project = template;
            self.composition = Default::default();
            self.layouts.clear();
            self.active_layout = None;
            // The finished candidate has its own retained overview and export
            // data. Keep the working preview only when it matches the template.
            if !self.preview_current() {
                self.preview = None;
                self.texture = None;
                self.layer_texture = None;
                self.map_colors = Default::default();
                self.terrain_projection = Default::default();
            }
        }
    }

    pub(super) fn with_recipe_draft(&mut self, draw: impl FnOnce(&mut Self)) {
        let Some(mut draft) = self.generation.run.as_mut().and_then(|run| run.template.take()) else {
            draw(self);
            return;
        };
        // The draft only changes here or through Undo/Redo, so an unchanged
        // history snapshot can be reused across panels and frames.
        let before = self.generation.draft_snapshot.take().unwrap_or_else(|| draft.clone());
        std::mem::swap(&mut self.project, &mut draft);
        let active_selection = self.selected;
        self.selected = self.generation.draft_target.and_then(|id| {
            self.project.targets.iter().position(|target| target.instance_id == id)
        });
        draw(self);
        self.generation.draft_target = self.selected
            .and_then(|index| self.project.targets.get(index))
            .map(|target| target.instance_id);
        self.selected = active_selection;
        std::mem::swap(&mut self.project, &mut draft);
        if before == draft {
            self.generation.draft_snapshot = Some(before);
        } else {
            self.history.record(before, &draft);
        }
        self.generation.run.as_mut().unwrap().template = Some(draft);
    }

    pub(super) fn undo_generation_draft(&mut self, redo: bool) -> bool {
        let Some(draft) = self.generation.run.as_mut().and_then(|run| run.template.as_mut()) else {
            return false;
        };
        self.generation.draft_snapshot = None;
        if redo {
            self.history.redo(draft);
        } else {
            self.history.undo(draft);
        }
        true
    }

    #[cfg(test)]
    pub(super) fn begin_generation(&mut self, plan: BatchPlan, ctx: &egui::Context) {
        self.submit_generation(self.recipe_tabs.active(), plan, Submission::Queue, ctx);
    }

    fn submit_generation(&mut self, id: u64, plan: BatchPlan, submission: Submission, ctx: &egui::Context) {
        if submission == Submission::Run
            && let Some(reason) = self.run_blocker()
        {
            self.status = reason.into();
            return;
        }
        let count = plan.count as u64;
        let origin = self.recipe_tabs.origin(id);
        match self.enqueue_generation_from(plan, ctx, false, Some(origin)) {
            Ok(first) => {
                self.generation.pending_tabs.extend(first..first + count);
                if submission == Submission::Run {
                    self.generation.execution.requested_parallel.extend(first..first + count);
                    self.start_requested_parallel(ctx);
                }
                self.show_tab(Tab::Generated(first));
                ctx.request_repaint();
            }
            Err(error) => self.status = error,
        }
    }

    #[cfg(test)]
    fn enqueue_generation(&mut self, plan: BatchPlan, ctx: &egui::Context, preserve_recipe: bool) -> Result<u64, String> {
        self.enqueue_generation_from(plan, ctx, preserve_recipe, None)
    }

    fn enqueue_generation_from(
        &mut self,
        mut plan: BatchPlan,
        ctx: &egui::Context,
        preserve_recipe: bool,
        origin: Option<recipe_tabs::RecipeOrigin>,
    ) -> Result<u64, String> {
        if self.drag_before.is_some() {
            return Err("Finish moving the recipe monument before generating.".into());
        }
        if (1..=MAX_BATCH).contains(&plan.count) && !self.generation.can_enqueue(plan.count) {
            return Err(if self.generation.available_slots() == 0 {
                GenerationBlocker::QueueFull.message().into()
            } else {
                "There are too few queue slots for this batch.".into()
            });
        }
        // Edits and requeued maps retain their exact recipe. Only new map
        // requests upgrade defaults or populate the roster.
        let working_recipe = !preserve_recipe
            && origin.as_ref().is_none_or(|origin| origin.id == self.recipe_tabs.active())
            && plan.recipe == *self.document_recipe();
        if !preserve_recipe {
            mapper_studio::generation_defaults::apply(&mut plan.recipe);
        }
        self.generation_preflight(
            &plan.recipe, working_recipe || (!preserve_recipe
                && origin.as_ref().is_none_or(|origin| origin.id == self.recipe_tabs.active())
                && plan.recipe == self.project),
        )?;
        if !preserve_recipe {
            let catalog = self.prefabs.catalog(&plan.recipe);
            self.monuments.refresh_saved_roster(&mut plan.recipe, catalog.as_deref())?;
        }
        if !preserve_recipe
            && let Some(origin) = &origin
            && self.recipe_tabs.active() != origin.id
        {
            self.recipe_tabs.parked_readiness(origin.id, &plan.recipe)?;
        }
        let recipes = plan.recipes()?;
        if self.generation.next_id.checked_add(recipes.len() as u64).is_none() {
            return Err("Map tab identifiers are exhausted".into());
        }
        if !self.generation.busy() {
            self.cancel_generation_work();
        }
        let excess = (self.generation.queue.len() + recipes.len()).saturating_sub(MAX_BATCH);
        self.generation.discard_queue_prefix(excess);
        self.generation.revision += 1;
        let first_id = self.generation.next_id;
        let first_index = self.generation.queue.len();
        for recipe in recipes {
            let id = self.generation.next_id;
            self.generation.next_id += 1;
            let seed = recipe.seeds[0].value;
            let duplicate = self
                .generation
                .results
                .iter()
                .any(|r| r.recipe.seeds[0].value == seed)
                || self
                    .generation
                    .queue
                    .iter()
                    .any(|r| r.recipe.seeds[0].value == seed);
            let label = if duplicate {
                format!("Seed {seed} · {id}")
            } else {
                format!("Seed {seed}")
            };
            self.generation.queue.push(QueueItem {
                id,
                label,
                recipe,
                state: State::Waiting,
                detail: String::new(),
                origin: origin.clone(),
            });
        }
        if self.generation.busy() {
            return Ok(first_id);
        }
        let first = &mut self.generation.queue[first_index];
        first.state = State::Running;
        let active_id = first.id;
        self.composition = Default::default();
        let randomized = plan.random_master || plan.randomize.iter().any(|&randomize| randomize);
        let before = std::mem::replace(&mut self.project, first.recipe.clone());
        self.generation.draft_target = self.selected
            .and_then(|index| before.targets.get(index))
            .map(|target| target.instance_id);
        self.skip_history = true;
        let template = if randomized || preserve_recipe || !working_recipe {
            before
        } else {
            self.history.record(before, &self.project);
            self.project.clone()
        };
        self.layouts.clear();
        self.active_layout = None;
        self.generation.run = Some(Run {
            start: first_index,
            active_id,
            template: Some(template),
            finished: first_index,
            terrain_ready: false,
        });
        self.topology_tool.enabled = false;
        self.topology_tool.cancel_stroke();
        if self.generation.queue_paused || self.primary_waits_for_capacity(&self.project) {
            self.generation.execution.start_pending = Some(preserve_recipe);
        } else {
            self.start_generation_candidate(ctx, preserve_recipe);
        }
        Ok(first_id)
    }

    fn start_generation_candidate(&mut self, ctx: &egui::Context, reuse_terrain: bool) {
        self.erosion_comparison.cancel();
        self.reserve_primary_budget();
        self.generation.execution.primary_paused = false;
        self.generation.execution.frozen_primary = None;
        self.generation.started = Some(Instant::now());
        self.clear_stage_preview();
        if reuse_terrain
            && self.preview_current()
            && !mapper_studio::native_preview::is_native(&self.project.preview_engine)
        {
            self.generation_terrain_ready();
            self.start_placement(ctx);
        } else {
            self.request_preview(ctx, reuse_terrain);
        }
    }

    fn apply_map_edit(&mut self, id: u64, ctx: &egui::Context) {
        let Some(edit) = self.generation.edits.get(&id).filter(|edit| {
            edit.has_changes() && !edit.dragging() && edit.job.is_none()
        }) else { return; };
        let Some(result) = self.generation.results.iter().find(|result| result.id == id) else {
            return;
        };
        let job = if mapper_studio::native_preview::is_native(&result.recipe.preview_engine)
            && !self.source.inspecting()
            && self.source.bound_root_ref(&result.recipe).is_none()
        {
            self.open_installation_settings();
            Err(GenerationBlocker::MatchingInstallation.message().into())
        } else {
            edit.recipe(&result.recipe, &result.monuments)
                .and_then(|recipe| {
                    let origin = self.generation.queue.iter().find(|item| item.id == id)
                        .and_then(|item| item.origin.clone());
                    self.enqueue_generation_from(BatchPlan::exact(recipe), ctx, true, origin)
                })
        };
        let edit = self.generation.edits.get_mut(&id).unwrap();
        match job {
            Ok(job) => {
                edit.job = Some(job);
                edit.last_issue = None;
                edit.apply_error = None;
            }
            Err(error) => edit.apply_error = Some(error),
        }
    }

    pub(super) fn cancel_generation(&mut self) {
        self.generation.queue_paused = false;
        self.generation.execution.drain_parallel = false;
        self.generation.execution.requested_parallel.clear();
        self.generation.execution.next_primary = None;
        self.generation.execution.advance_pending = false;
        self.generation.execution.start_pending = None;
        self.generation.execution.placement_pending = false;
        self.generation.execution.primary_paused = false;
        self.record_generation_elapsed();
        let active: Vec<_> = self.generation.execution.jobs.keys().copied().collect();
        for id in active {
            self.cancel_extra_queue_job(id);
        }
        if self.generation.run.is_some() {self.generation.revision+=1;}
        for item in &mut self.generation.queue {
            if matches!(item.state, State::Running | State::Waiting) {
                item.state = State::Cancelled;
            }
        }
        for edit in self.generation.edits.values_mut() {
            edit.job = None;
        }
        self.cancel_generation_work();
        self.finish_generation_run();
    }
    fn cancel_queued_map(&mut self, id: u64, ctx: &egui::Context) {
        if self.cancel_extra_queue_job(id) {
            ctx.request_repaint();
            return;
        }
        let Some(index) = self.generation.queue.iter().position(|item| item.id == id) else { return; };
        if self.generation.queue[index].state == State::Waiting
            && self.generation.run.as_ref().is_none_or(|run| index > run.finished)
        {
            self.generation.queue.remove(index);
            self.generation.retire_waiting_map(id);
            self.generation.revision += 1;
            ctx.request_repaint();
            return;
        }
        let Some(run) = &mut self.generation.run else { return; };
        let item = &mut self.generation.queue[index];
        match item.state {
            State::Running if run.active_id == id => {
                item.state = State::Cancelled;
            }
            _ => return,
        }
        self.generation.revision += 1;
        if let Some(edit) = self.generation.edits.values_mut().find(|edit| edit.job == Some(id)) {
            edit.job = None;
        }
        self.cancel_generation_work();
        self.advance_generation(ctx);
    }

    fn run_queued_map_next(&mut self, id: u64, ctx: &egui::Context) {
        let Some(next) = self.generation.queue.iter().find(|item| item.state == State::Waiting) else {
            return;
        };
        self.move_waiting_map(id, next.id, ctx);
        if self.generation.queue.iter().any(|item| item.id == id && item.state == State::Waiting) {
            self.generation.execution.next_primary = Some(id);
        }
    }

    fn move_waiting_map(&mut self, id: u64, target: u64, ctx: &egui::Context) {
        let Some(run) = &self.generation.run else { return };
        let next = run.finished + 1;
        let waiting_index = |id| self.generation.queue.iter().enumerate().skip(next)
            .find_map(|(index, item)| (item.id == id && item.state == State::Waiting).then_some(index));
        let (Some(from), Some(to)) = (waiting_index(id), waiting_index(target)) else {
            return;
        };
        if from == to {
            return;
        }
        if from < to {
            self.generation.queue[from..=to].rotate_left(1);
        } else {
            self.generation.queue[to..=from].rotate_right(1);
        }
        self.generation.execution.next_primary = None;
        self.generation.revealed_job = Some(run.active_id);
        self.generation.queue_focus = Some(id);
        self.generation.revision += 1;
        ctx.request_repaint();
    }

    fn step_waiting_map(&mut self, id: u64, direction: isize, ctx: &egui::Context) {
        let waiting: Vec<_> = self.generation.queue.iter().filter(|item| item.state == State::Waiting).map(|item| item.id).collect();
        let Some(index) = waiting.iter().position(|&job| job == id)
            .and_then(|index| index.checked_add_signed(direction)) else { return };
        if let Some(&target) = waiting.get(index) { self.move_waiting_map(id, target, ctx); }
    }

    fn cancel_generation_work(&mut self) {
        self.generation.overview=None;
        self.generation.overview_ready=None;
        self.map_archive.cancel_build();
        self.task = None;
        self.placement_task = None;
        self.composition.cancel_pending();
        self.clear_stage_preview();
        self.generation_progress = None;
    }
    pub(super) fn report_generation_worker_failure(&mut self, reason: String, ctx: &egui::Context) {
        if self.generation.active_recipe() == Some(&self.project) {
            self.fail_generation(reason, ctx);
        } else {
            self.status = reason;
        }
    }

    fn fail_generation(&mut self, reason: String, ctx: &egui::Context) {
        self.generation.revision+=1;
        let active = self.generation.run.as_ref().map(|r| r.active_id);
        self.cancel_generation_work();
        if let Some(item) = self
            .generation
            .queue
            .iter_mut()
            .find(|q| Some(q.id) == active)
        {
            item.state = State::Failed;
            item.detail = reason.clone();
            retain_failed_recipe(self.settings_path.as_deref(), item);
        }
        if let Some(id) = active
            && let Some(edit) = self.generation.edits.values_mut().find(|edit| edit.job == Some(id))
        {
            edit.job = None;
            edit.last_issue = Some(EditIssue { id, incomplete: false, detail: reason.clone() });
        }
        self.status = reason;
        self.advance_generation(ctx);
    }
    fn advance_generation(&mut self, ctx: &egui::Context) {
        self.retire_primary_budget();
        self.generation.execution.start_pending = None;
        self.generation.execution.primary_paused = false;
        self.generation.execution.placement_pending = false;
        self.record_generation_elapsed();
        self.generation.overview=None;
        self.generation.overview_ready=None;
        let Some(run) = &self.generation.run else { return; };
        let mut waiting = self.generation.queue.iter().enumerate().skip(run.finished + 1)
            .filter(|(_, item)| item.state == State::Waiting);
        if let Some((next, _)) = waiting.clone().next() {
            let priority = self.generation.execution.next_primary;
            let selected = priority.and_then(|id| {
                waiting.clone().find_map(|(index, item)| (item.id == id).then_some(index))
            }).or_else(|| waiting.find_map(|(index, item)| {
                self.generation.execution.requested_parallel.contains(&item.id).then_some(index)
            })).unwrap_or(next);
            if self.generation.queue_paused
                || self.primary_waits_for_capacity(&self.generation.queue[selected].recipe)
            {
                self.generation.execution.advance_pending = true;
                if !self.generation.queue_paused {
                    ctx.request_repaint_after(Duration::from_millis(120));
                }
                return;
            }
            self.generation.execution.next_primary = None;
            let run = self.generation.run.as_mut().unwrap();
            // Keep the ordinary waiting jobs in their existing order and inside
            // the unconsumed portion of the primary run.
            if selected != next {
                self.generation.queue[next..=selected].rotate_right(1);
            }
            let item = &mut self.generation.queue[next];
            self.generation.execution.requested_parallel.remove(&item.id);
            let reuse_terrain = run.terrain_ready
                || self.generation.edits.values().any(|edit| edit.job == Some(item.id));
            item.state = State::Running;
            run.active_id = item.id;
            run.finished = next;
            run.terrain_ready = false;
            self.project.clone_from(&item.recipe);
            self.composition = Default::default();
            self.map_archive.cancel_build();
            self.layouts.clear();
            self.active_layout = None;
            self.start_generation_candidate(ctx, reuse_terrain);
        } else {
            let items = &self.generation.queue[run.start..];
            let ready = items.iter().filter(|q| q.state == State::Ready).count();
            let failed = items.iter().filter(|q| q.state == State::Failed).count();
            let incomplete = items.iter().filter(|q| q.state == State::Incomplete).count();
            let cancelled = items.iter().filter(|q| q.state == State::Cancelled).count();
            let count = items.len();
            self.finish_generation_run();
            if failed == 0 && incomplete == 0 && cancelled == 0 {
                self.status = format!("{ready} {} ready.", if ready == 1 { "map" } else { "maps" });
            } else if count > 1 || incomplete > 0 || cancelled > 0 {
                let counts = [(ready, "ready"), (incomplete, "incomplete"), (failed, "failed"), (cancelled, "cancelled")]
                    .into_iter().filter(|(count, _)| *count > 0)
                    .map(|(count, label)| format!("{count} {label}")).collect::<Vec<_>>();
                self.status = format!("{}.", counts.join(" · "));
            }
        }
    }
    pub(super) fn generation_in_progress(&self) -> bool {
        self.generation.busy()
            || self.task.is_some()
            || self.placement_task.is_some()
            || self.composition.running()
            || !self.generation.execution.jobs.is_empty()
            || self.generation.execution.retiring.iter().any(mapper_studio::generation_resources::JobBudget::has_workers)
    }

    pub(super) fn deferred_generation_placement(&self) -> bool {
        self.generation.run.as_ref().is_some_and(|run| run.terrain_ready)
            && self.generation.active_recipe() == Some(&self.project)
            && mapper_studio::composition::selects_monument_layout(&self.project)
    }

    pub(super) fn generation_terrain_ready(&mut self) {
        if self.generation.active_recipe() == Some(&self.project)
            && let Some(run) = &mut self.generation.run
        {
            run.terrain_ready = true;
        }
    }

    fn generation_stage(&self) -> (Cow<'static, str>, Option<f32>) {
        if self.task.is_none() && self.placement_task.is_none()
            && let Some((stage, fraction)) = self.composition.progress()
        {
            return (stage.into(), fraction);
        }
        let (stage, fraction) = if self.task.is_some() {
            (self.generation_progress.unwrap_or("Preparing terrain"), None)
        } else if self.placement_task.is_some() {
            ("Placing monuments", None)
        } else if self.composition.running() {
            ("Composing map", None)
        } else if self.map_archive.preparing() {
            ("Saving map", None)
        } else if self.map_colors.running() {
            ("Rendering map colors", None)
        } else {
            ("Preparing map", None)
        };
        (stage.into(), fraction)
    }

    fn generation_blocker(&mut self, ui: &mut egui::Ui, blocker: &GenerationBlocker) {
        match blocker {
            GenerationBlocker::MatchingInstallation | GenerationBlocker::UnsupportedNativeProfile(_) => {
                if ui.button("Rust installations…").on_hover_text(blocker.message()).clicked() {
                    self.open_installation_settings();
                }
            }
            GenerationBlocker::AssetsUnavailable => {
                if ui.button("Retry asset loading").on_hover_text(blocker.message()).clicked() {
                    self.prefabs.discover(&self.project, self.source.bound_root(&self.project), ui.ctx());
                }
            }
            GenerationBlocker::Overlay { anchor, message } => {
                if ui.button("Fix layout…").on_hover_text(message).clicked() {
                    self.with_recipe_draft(|studio| {
                        let recipe = &studio.project;
                        let identities = studio.source.bound_root_ref(recipe)
                            .and_then(|root| studio.prefabs.prepared_identities(recipe, root));
                        let path = identities.as_ref().and_then(|ids| ids.prefab_path(*anchor))
                            .unwrap_or_default().to_owned();
                        let name = studio.monument_icons.name(Some(&path), "Monument layout");
                        studio.custom_monuments.close();
                        studio.monument_dialog.page = super::monument_dialog::Page::Layouts;
                        studio.content.open_overlay_recovery(recipe, studio.recipe_tabs.editor(), *anchor, path, name, message.clone());
                    });
                }
                ui.ctx().request_repaint_after(std::time::Duration::from_secs(2));
            }
            GenerationBlocker::CheckingOverlays => {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
                loading_ui::show(ui, loading_ui::Status::Blocked(blocker.message()));
            }
            GenerationBlocker::QueueFull => {
                ui.weak("Queue full").on_hover_text(blocker.message());
            }
            _ => loading_ui::show(ui, loading_ui::Status::Blocked(blocker.message())),
        }
        if !self.generation_in_progress() && matches!(blocker,
            GenerationBlocker::MatchingInstallation
                | GenerationBlocker::UnsupportedNativeProfile(_)
                | GenerationBlocker::AssetsUnavailable)
        {
            if ui.available_width() >= 127. {
                diagnostic_ui::copy_error(ui, blocker.message(), None);
            } else {
                ui.menu_button("Details", |ui| {
                    ui.label(diagnostic_ui::summary(blocker.message()).as_ref());
                    diagnostic_ui::copy_error(ui, blocker.message(), None);
                });
            }
        }
    }

    #[cfg(test)]
    pub(super) fn recipe_generate(&mut self, ui: &mut egui::Ui) {
        self.recipe_generate_for(ui, self.recipe_tabs.active());
    }

    pub(super) fn recipe_generate_for(&mut self, ui: &mut egui::Ui, id: u64) {
        let readiness = self.generation_readiness_for(id);
        let blocker = readiness.as_ref().err();
        let preparation = self.source.inspection_token()
            .or_else(|| self.prefabs.preparation_token()).cloned();
        let recovery = blocker.filter(|error| !matches!(error, GenerationBlocker::MissingSeed));
        let show_setup = preparation.is_some()
            && !matches!(recovery, Some(GenerationBlocker::MatchingInstallation
                | GenerationBlocker::UnsupportedNativeProfile(_)
                | GenerationBlocker::AssetsUnavailable | GenerationBlocker::Overlay { .. }));
        egui::Panel::bottom(recipe_tabs::seed_id("recipe-generate", id)).exact_size(44.).resizable(false)
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(4, 5)))
            .show(ui, |ui| {
                let width = (ui.available_width() * 0.92).clamp(1., 260.);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.;
                    ui.add_space(((ui.available_width() - width) * 0.5).max(0.));
                    self.generate_split_button(ui, width, readiness.is_ok(), blocker, id);
                });
            });
        if show_setup || recovery.is_some() {
            egui::Panel::bottom(recipe_tabs::seed_id("recipe-readiness", id)).exact_size(ui.spacing().interact_size.y)
                .resizable(false).frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(4, 0)))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if let Some(token) = preparation.as_ref().filter(|_| show_setup) {
                            loading_ui::show(ui, loading_ui::Status::Preparing(token));
                        } else if let Some(error) = recovery {
                            let ctx = ui.ctx().clone();
                            self.with_recipe_editor(id, &ctx, |studio| studio.generation_blocker(ui, error));
                        }
                    });
                });
        }
    }

    fn generate_split_button(
        &mut self,
        ui: &mut egui::Ui,
        width: f32,
        enabled: bool,
        blocker: Option<&GenerationBlocker>,
        id: u64,
    ) {
        const HEIGHT: f32 = 34.;
        const RADIUS: u8 = 8;
        let batch_enabled = enabled || matches!(blocker, Some(GenerationBlocker::QueueFull));
        let can_queue = enabled || matches!(blocker, Some(GenerationBlocker::MissingSeed));
        let run_blocker = self.run_blocker();
        let can_run = can_queue && run_blocker.is_none();
        let run_blue = enabled && run_blocker.is_none();
        let left = egui::CornerRadius { nw: RADIUS, sw: RADIUS, ne: 0, se: 0 };
        let right = egui::CornerRadius { nw: 0, sw: 0, ne: RADIUS, se: RADIUS };
        let (queue, run, batch) = ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.;
            let gap = 6_f32.min(width * 0.05);
            let queue_width = (width - gap) * 0.38;
            let batch_width = 32_f32.min(width * 0.17);
            let run_width = (width - queue_width - gap - batch_width).max(0.);
            let queue = ui.add_enabled(
                can_queue,
                egui::Button::new("Queue")
                    .min_size(egui::vec2(queue_width, HEIGHT))
                    .corner_radius(RADIUS)
                    .truncate(),
            );
            ui.add_space(gap);
            let background = ui.painter().add(egui::Shape::Noop);
            let muted_light = !run_blue && !ui.visuals().dark_mode;
            let label_color = if muted_light { ui.visuals().weak_text_color() } else { Color32::WHITE };
            let run_button = egui::Button::new(egui::RichText::new("Generate").strong().color(label_color))
                .min_size(egui::vec2(run_width, HEIGHT))
                .truncate()
                .frame(!run_blue)
                .corner_radius(left);
            let run = if muted_light {
                ui.scope(|ui| {
                    ui.visuals_mut().disabled_alpha = 1.;
                    ui.add_enabled(can_run, run_button)
                }).inner
            } else {
                ui.add_enabled(can_run, run_button)
            };
            let batch = ui.add_enabled(
                batch_enabled,
                egui::Button::new("")
                    .min_size(egui::vec2(batch_width, HEIGHT))
                    .frame(!batch_enabled)
                    .corner_radius(right),
            );
            if run_blue {
                ui.painter().set(background, workbench_style::generate_fill(
                    ui.ctx(), run.rect.union(batch.rect), egui::CornerRadius::same(RADIUS), 0.,
                ));
            } else if batch_enabled {
                ui.painter().set(background, workbench_style::generate_fill(ui.ctx(), batch.rect, right, 0.8));
            }
            if run_width >= 103. {
                let at = egui::pos2(run.rect.left() + 12., run.rect.center().y);
                ui.painter().add(egui::Shape::convex_polygon(
                    vec![at + egui::vec2(-2., -4.), at + egui::vec2(5., 0.), at + egui::vec2(-2., 4.)],
                    label_color,
                    egui::Stroke::NONE,
                ));
            }
            batch.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, batch_enabled, "Batch generation"));
            let divider = if run_blue {
                Color32::from_white_alpha(110)
            } else {
                ui.visuals().weak_text_color().gamma_multiply(0.35)
            };
            ui.painter().vline(batch.rect.left(), batch.rect.y_range(), egui::Stroke::new(1., divider));
            for (button, corners) in [(&run, left), (&batch, right)] {
                let stroke = if button.has_focus() {
                    ui.visuals().selection.stroke
                } else if button.hovered() && button.enabled() {
                    egui::Stroke::new(1., Color32::from_white_alpha(85))
                } else {
                    continue;
                };
                ui.painter().rect_stroke(button.rect.shrink(1.), corners - 1, stroke, egui::StrokeKind::Inside);
            }
            (queue, run, batch)
        }).inner;
        let action_hint = |help: &'static str| {
            if matches!(blocker, Some(GenerationBlocker::MissingSeed)) {
                mapper_studio::MASTER_SEED_REQUIRED
            } else {
                help
            }
        };
        let queue = queue.on_hover_text(action_hint("Add one map to the end of the queue."));
        let run = run.on_hover_text(action_hint("Start one map immediately."));
        let batch = batch.on_hover_text("Batch generation");
        if let Some(blocker) = blocker {
            queue.clone().on_disabled_hover_text(blocker.message());
            if !batch_enabled {
                batch.clone().on_disabled_hover_text(blocker.message());
            }
        }
        if let Some(reason) = blocker.map(GenerationBlocker::message).or(run_blocker) {
            run.clone().on_disabled_hover_text(reason);
        }
        if queue.clicked() {
            self.generate_recipe_for(id, Submission::Queue, ui.ctx());
        }
        if run.clicked() {
            self.generate_recipe_for(id, Submission::Run, ui.ctx());
        }
        let screen = ui.ctx().content_rect();
        let popup_width = (screen.width() - 16.).clamp(120., 364.);
        let trigger = batch.rect.center().x;
        let anchor = Rect::from_min_size(
            egui::pos2(trigger - popup_width * 0.27, batch.rect.top()),
            egui::vec2(popup_width, batch.rect.height()),
        );
        let popup_frame = egui::Frame::popup(ui.style())
            .inner_margin(egui::Margin::symmetric(20, 14))
            .corner_radius(12)
            .stroke(egui::Stroke::new(
                1.,
                ui.visuals().weak_text_color().lerp_to_gamma(ui.visuals().window_fill, 0.57),
            ));
        let popup = egui::Popup::menu(&batch)
            .anchor(anchor)
            .align(egui::RectAlign::TOP_START)
            .align_alternatives(&[])
            .gap(22.)
            .width(popup_width)
            .frame(popup_frame)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
        let popup_id = popup.get_id();
        if batch_enabled {
            if let Some(open) = popup.show(|ui| {
                self.batch_menu(ui, id, popup_width - popup_frame.total_margin().sum().x)
            }) {
                let rect = open.response.rect;
                let x = trigger.clamp(rect.left() + 34., rect.right() - 34.);
                let top = rect.bottom();
                if top + 17. <= batch.rect.top() {
                    let painter = ui.ctx().layer_painter(open.response.layer_id);
                    let left = egui::pos2(x - 22., top - 1.5);
                    let tip = egui::pos2(x, top + 17.);
                    let right = egui::pos2(x + 22., top - 1.5);
                    painter.add(egui::Shape::convex_polygon(
                        vec![left, tip, right],
                        popup_frame.fill,
                        egui::Stroke::NONE,
                    ));
                    painter.add(egui::Shape::line(
                        vec![left + egui::vec2(1.8, 1.5), tip, right + egui::vec2(-1.8, 1.5)],
                        popup_frame.stroke,
                    ));
                }
            }
        } else {
            egui::Popup::close_id(ui.ctx(), popup_id);
        }
        let center = batch.rect.center();
        let direction = if egui::Popup::is_id_open(ui.ctx(), popup_id) { -1. } else { 1. };
        let arrow_color = if batch_enabled { Color32::WHITE } else { ui.visuals().weak_text_color() };
        ui.painter().add(egui::Shape::convex_polygon(
            vec![
                center + egui::vec2(-4. * direction, -2. * direction),
                center + egui::vec2(4. * direction, -2. * direction),
                center + egui::vec2(0., 3. * direction),
            ],
            arrow_color,
            egui::Stroke::NONE,
        ));
    }

    fn batch_menu(&mut self, ui: &mut egui::Ui, id: u64, width: f32) {
        ui.set_width(width);
        ui.spacing_mut().item_spacing.y = 7.;
        ui.spacing_mut().interact_size.y = 30.;
        for text_style in [egui::TextStyle::Body, egui::TextStyle::Button] {
            ui.style_mut().text_styles.insert(text_style, egui::FontId::proportional(15.));
        }
        let compact = width < 275.;
        let control_width = if compact { width } else { 170. };

        ui.label(egui::RichText::new("Batch generation").size(18.).strong());
        ui.add_space(7.);
        let count = &mut self.generation.count;
        let mut stepper = |ui: &mut egui::Ui, label: egui::Id| {
            let fill = ui.visuals().widgets.inactive.weak_bg_fill;
            let stroke = ui.visuals().widgets.inactive.bg_stroke;
            egui::Frame::new().fill(fill).stroke(stroke).corner_radius(8).show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 0.;
                let step_w = (control_width * 0.24).min(36.);
                let minus = egui::Button::new("−")
                    .frame_when_inactive(false)
                    .corner_radius(egui::CornerRadius { nw: 7, sw: 7, ne: 0, se: 0 })
                    .min_size(egui::vec2(step_w, 30.));
                if ui.add_enabled(*count > 1, minus).on_hover_text("Remove one map").clicked() {
                    *count -= 1;
                }
                ui.scope(|ui| {
                    let visuals = ui.visuals_mut();
                    let value_fill = visuals.extreme_bg_color;
                    for widget in [&mut visuals.widgets.inactive, &mut visuals.widgets.hovered, &mut visuals.widgets.active] {
                        widget.corner_radius = egui::CornerRadius::ZERO;
                        widget.weak_bg_fill = value_fill;
                    }
                    ui.add_sized(
                        [control_width - step_w * 2. - stroke.width * 2., 30.],
                        egui::DragValue::new(count).range(1..=MAX_BATCH),
                    ).labelled_by(label);
                });
                let plus = egui::Button::new("+")
                    .frame_when_inactive(false)
                    .corner_radius(egui::CornerRadius { nw: 0, sw: 0, ne: 7, se: 7 })
                    .min_size(egui::vec2(step_w, 30.));
                if ui.add_enabled(*count < MAX_BATCH, plus).on_hover_text("Add one map").clicked() {
                    *count += 1;
                }
            });
        };
        if compact {
            let label = ui.label("Maps");
            ui.horizontal(|ui| stepper(ui, label.id));
        } else {
            ui.horizontal(|ui| {
                let label = ui.label("Maps");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        stepper(ui, label.id);
                    });
                });
            });
        }

        let seed_mode = ui.id().with("batch-random-master");
        let mut random_master = ui.data(|data| data.get_temp::<bool>(seed_mode)).unwrap_or(true);
        let mut seed_combo = |ui: &mut egui::Ui, label: egui::Id| {
            egui::ComboBox::from_id_salt("batch-seed-mode")
                .width(control_width)
                .selected_text(if random_master { "Master seed" } else { "Selected stages" })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut random_master, true, "Master seed");
                    ui.selectable_value(&mut random_master, false, "Selected stages");
                })
                .response.labelled_by(label);
        };
        if compact {
            let label = ui.label("Seed variation");
            seed_combo(ui, label.id);
        } else {
            ui.horizontal(|ui| {
                let label = ui.label("Seed variation");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    seed_combo(ui, label.id);
                });
            });
        }
        ui.data_mut(|data| data.insert_temp(seed_mode, random_master));
        let available = available_seed_domains(self.recipe_by_id(id));
        ui.add_space(2.);
        egui::CollapsingHeader::new("Vary stage seeds")
            .default_open(!random_master)
            .icon(|ui, open, response| {
                let rect = response.rect.shrink(3.);
                let visuals = ui.style().interact(response);
                ui.painter().rect_filled(rect, 5, ui.visuals().faint_bg_color);
                let points = [egui::vec2(-2., -4.), egui::vec2(2., 0.), egui::vec2(-2., 4.)];
                let rotate = egui::emath::Rot2::from_angle(open * std::f32::consts::FRAC_PI_2);
                ui.painter().add(egui::Shape::line(
                    points.into_iter().map(|point| rect.center() + rotate * point).collect(),
                    egui::Stroke::new(1.3, visuals.fg_stroke.color),
                ));
            })
            .show(ui, |ui| {
                ui.spacing_mut().interact_size.y = 22.;
                ui.spacing_mut().item_spacing.y = 4.;
                let mut show_stages = |ui: &mut egui::Ui| {
                    for (i, name) in DOMAINS.iter().enumerate() {
                        ui.add_enabled(available[i], egui::Checkbox::new(&mut self.generation.randomize[i], *name))
                            .on_hover_text("Choose a separate seed for this stage in each map.");
                    }
                };
                let available_height = (ui.ctx().content_rect().height() - 300.).max(55.);
                let stages_height = DOMAINS.len() as f32 * 26.;
                if available_height < stages_height {
                    egui::ScrollArea::vertical().max_height(available_height).show(ui, show_stages);
                } else {
                    show_stages(ui);
                }
            });

        let count = self.generation.count;
        let randomize = std::array::from_fn(|i| available[i] && self.generation.randomize[i]);
        let slots = self.generation.available_slots();
        let has_variation = count == 1 || has_varying_seeds(self.recipe_by_id(id), random_master, randomize);
        let can_queue = count <= slots && has_variation;
        let run_blocker = self.run_blocker();
        let can_run = can_queue && run_blocker.is_none();
        ui.add_space(5.);
        ui.separator();
        if count > slots {
            let label = match slots {
                0 => "Queue full".into(),
                1 => "1 slot available".into(),
                _ => format!("{slots} slots available"),
            };
            ui.weak(label).on_hover_text("Lower the map count, wait for a map to finish, or remove one in Queue.");
        } else if !has_variation {
            ui.label(NO_SEED_VARIATION);
        }
        ui.add_space(3.);
        let submission = ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.;
            let button_width = ((ui.available_width() - ui.spacing().item_spacing.x) / 2.).max(0.);
            let size = egui::vec2(button_width, 40.);
            let queue = ui.add_enabled(can_queue, egui::Button::new(format!("Queue {count}")).min_size(size).corner_radius(8).stroke(ui.visuals().window_stroke))
                .on_hover_text("Add the whole batch to the normal queue.");
            let mut button = egui::Button::new(if can_run {
                egui::RichText::new(format!("Run {count}")).color(Color32::WHITE)
            } else {
                egui::RichText::new(format!("Run {count}"))
            }).min_size(size).corner_radius(8);
            if can_run {
                button = button.fill(workbench_style::PRIMARY_ACTION);
            }
            let run = ui.add_enabled(can_run, button)
                .on_hover_text("Start this batch using available generation slots.");
            if let Some(reason) = run_blocker {
                run.clone().on_disabled_hover_text(reason);
            }
            if queue.clicked() { Some(Submission::Queue) }
            else if run.clicked() { Some(Submission::Run) }
            else { None }
        }).inner;
        if let Some(submission) = submission {
            let plan = BatchPlan {
                count,
                nonce: RandomState::new().hash_one(()),
                random_master,
                randomize,
                ..BatchPlan::exact(self.recipe_by_id(id).clone())
            };
            ui.close();
            self.submit_generation(id, plan, submission, ui.ctx());
        }
    }
    pub(super) fn tick_generation(&mut self, ctx: &egui::Context) {
        self.tick_queue_execution(ctx);
        let budget = self.generation.execution.primary_budget.clone();
        mapper_studio::generation_resources::with_job_budget(budget, || self.tick_primary_generation(ctx));
    }

    fn tick_primary_generation(&mut self, ctx: &egui::Context) {
        if !self.generation.busy() || self.queue_primary_paused()
            || self.generation.execution.start_pending.is_some()
            || self.generation.execution.advance_pending
        {
            return;
        }
        if self.generation.active_recipe() != Some(&self.project) {
            self.cancel_generation();
            self.status = "Recipe changed; generation stopped.".into();
            return;
        }
        if self.task.is_some() || self.placement_task.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        }
        let layout_ready = self.current_layout().is_some();
        if !layout_ready && !self.deferred_generation_placement() {
            let reason = self.status.clone();
            self.fail_generation(format!("Generation stopped: {reason}"), ctx);
            return;
        }
        if self.project.composition_engine.is_some()
            && mapper_studio::native_preview::is_native(&self.project.preview_engine)
        {
            match self.composition.completion(&self.project) {
                Ok(false) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                    return;
                }
                Err(error) => {
                    self.fail_generation(error, ctx);
                    return;
                }
                Ok(true) => {}
            }
        }
        if !layout_ready {
            self.fail_generation("Composition finished without a monument layout".into(), ctx);
            return;
        }
        // A generated tab must capture the same presentation as the working
        // viewport, even when that viewport is hidden during batch generation.
        self.refresh_map_colors(ctx);
        if let Some((base, palette)) = self.map_color_source() {
            match self.map_colors.completion_for(&base, &palette, &self.preferences.map_colors) {
                Ok(false) => {ctx.request_repaint_after(std::time::Duration::from_millis(100));return;}
                Err(error) => {self.fail_generation(error, ctx);return;}
                Ok(true) => {}
            }
        }
        let id = self.generation.run.as_ref().unwrap().active_id;
        let mut archive=None;
        let mut archive_issue=String::new();
        let mut archive_pending = false;
        if let (Some(view),Some(root),Some(catalog))=(self.composition.view(&self.project),self.source.bound_root_ref(&self.project),self.prefabs.catalog(&self.project)) {
            let identities = self.prefabs.prepared_identities(&self.project, root);
            let assets = map_archive::Assets { root: root.to_owned(), catalog, identities };
            match self.map_archive.prepare(id,view,assets,ctx) {
                None => archive_pending = true,
                Some(Ok(value))=>archive=Some(value),
                Some(Err(error))=>archive_issue=error,
            }
        }
        if archive_pending && self.preferences.generation_resources.effective_workers() == 1 {
            self.status = "Saving generated map…".into();
            return;
        }
        let overview=match self.retain_generated_overview(id,ctx) {
            Ok(overview)=>overview,
            Err(error)=> {self.fail_generation(error,ctx);return;},
        };
        if archive_pending {
            self.status = "Saving generated map…".into();
            return;
        }
        let Some(overview) = overview else {
            self.status = "Retaining map overview…".into();
            ctx.request_repaint_after(std::time::Duration::from_millis(80));
            return;
        };
        let Some((image, texture)) = self.exportable_overview() else {
            self.fail_generation("Generation produced no accepted image".into(), ctx);
            return;
        };
        let texture = texture.cloned().unwrap_or_else(|| {
            ctx.load_texture(
                format!("generated-map-{id}"),
                image.clone(),
                egui::TextureOptions::LINEAR,
            )
        });
        let layout = self.current_layout().unwrap();
        let mut monuments = map_monuments::initial(&self.project, &layout.solution);
        let missing = layout.solution.required_missing;
        let placement_issue = if missing > 0 {
            mapper_studio::generation_validation::required_monuments_issue(
                &self.project,
                &layout.solution,
            )
        } else {
            None
        };
        let issue = placement_issue.or_else(|| {
            completion_issue(&self.project, missing, archive.is_some(), &archive_issue)
        });
        let complete = issue.is_none();
        let edited_map = self.generation.edits.iter().find_map(|(&map_id, edit)| {
            (edit.job == Some(id)).then_some(map_id)
        });
        let replacing = edited_map.filter(|_| complete);
        // A project can contain one extra map saved from the working preview.
        let result_limit = if replacing.is_some() {
            MAX_BATCH.max(self.generation.results.len())
        } else {
            MAX_BATCH
        };
        let needed = (self.generation.results.len() + 1).saturating_sub(result_limit);
        if needed > 0 {
            let available = self.generation.results.iter().filter(|result| {
                replacing == Some(result.id) || self.generation.edits.get(&result.id)
                    .is_none_or(|edit| !edit.protects_result())
            }).take(needed).count();
            if available < needed {
                self.fail_generation("Map storage is full. Apply or discard a draft to free a slot.".into(), ctx);
                return;
            }
        }
        let item = self
            .generation
            .queue
            .iter_mut()
            .find(|q| q.id == id)
            .unwrap();
        item.state = if complete { State::Ready } else { State::Incomplete };
        item.detail = issue.unwrap_or_default();
        if archive_issue.is_empty() && item.state == State::Incomplete {
            archive_issue.clone_from(&item.detail);
        }
        if item.state == State::Incomplete {
            retain_failed_recipe(self.settings_path.as_deref(), item);
        }
        let label = item.label.clone();
        let mut view = None;
        let id = if let Some(map_id) = replacing {
            self.generation.edits.remove(&map_id);
            self.generation.road_layouts.remove(&map_id);
            if let Some(index) = self.generation.results.iter().position(|r| r.id == map_id) {
                let old = self.generation.results.remove(index);
                view = Some((old.label, old.zoom, old.pan, old.grid, old.no_build));
            }
            // Keep the existing tab, including its dock position and selection.
            let run = self.generation.run.as_mut().unwrap();
            if let Some(index) = self.generation.queue.iter().position(|q| q.id == map_id) {
                self.generation.queue.remove(index);
                run.remove_earlier_item(index);
            }
            run.active_id = map_id;
            let item = &mut self.generation.queue[run.finished];
            item.id = map_id;
            if let Some((label, ..)) = &view {
                item.label.clone_from(label);
            }
            map_id
        } else {
            if let Some(map_id) = edited_map {
                let edit = self.generation.edits.get_mut(&map_id).unwrap();
                edit.job = None;
                edit.last_issue = EditIssue::from_item(item);
            }
            id
        };
        while self.generation.results.len() >= result_limit {
            let index = self.map_eviction_index()
                .expect("an undrafted map can be replaced");
            let old = self.generation.results.remove(index);
            self.generation.edits.remove(&old.id);
            self.generation.road_layouts.remove(&old.id);
            self.generation.elapsed.remove(&old.id);
            // A finished queue card can outlive its in-memory result.
            let run = self.generation.run.as_mut().unwrap();
            if let Some(index) = self.generation.queue.iter().position(|q| q.id == old.id) {
                self.generation.queue.remove(index);
                run.remove_earlier_item(index);
            }
            self.generation.resident_previews.retain(|&id| id != old.id);
            self.remove_generated_tab(old.id);
        }
        let routes = self.composition.view(&self.project).map_or_else(Vec::new, |view| {
            monuments.extend(view.generated_monuments.iter().cloned());
            mapper_studio::path_edit::capture(&self.project, &view.maps)
        });
        for monument in &mut monuments {monument.name=self.monument_icons.name(monument.prefab.as_deref(),&monument.name);}
        let placed = monuments.iter().filter(|marker| !marker.is_scenery_marker()).count();
        let identity=self.current_map_identity();
        let (label, zoom, pan, grid, no_build) =
            view.unwrap_or((label, 1., Vec2::ZERO, self.grid, self.show_no_build));
        if complete && let Some(ring) = self.composition.view(&self.project).and_then(composition_panel::View::accepted_road_layout) {
            self.generation.road_layouts.insert(id, Arc::new(ring));
        }
        self.generation.results.push(Candidate {
            identity,
            overview:Some(overview),
            archive,archive_issue,
            id,
            label,
            recipe: self.project.clone(),
            image: Some(Ok(texture)),
            zoom,
            pan,
            placed,
            missing,
            monuments,
            routes,
            restrictions: self.restrictions.for_export(),
            grid,
            no_build,
            map_font: self.monument_icons.labels.source(self.project.installation.as_ref()),
            background: self.viewport_background(),
            palette: self.composition.view(&self.project).and_then(|view|view.palette.as_ref()).map(|p|std::sync::Arc::new(p.with_display_colors(&self.preferences.map_colors))),
        });
        self.generation.remember_preview(id);
        self.generation.revision+=1;
        // Keep the current map focused while new results open alongside it.
        if edited_map.is_none()
            && !self.generation.closed_job_tabs.remove(&id)
            && self.dock.find_tab(&Tab::Generated(id)).is_none()
            && !self.generation.pending_tabs.contains(&id)
        {
            if let Some(active) = self.visible_generated_tab()
                && let Some(path) = self.dock.find_tab(&Tab::Generated(active))
                && let Ok(leaf) = self.dock.leaf_mut(path.node_path())
            {
                leaf.tabs.push(Tab::Generated(id));
            } else {
                self.show_tab(Tab::Generated(id));
            }
        }
        self.advance_generation(ctx);
    }

    fn map_eviction_index(&self) -> Option<usize> {
        let focused = self.visible_generated_tab();
        self.generation.results.iter().enumerate().filter(|(_, result)| {
            self.generation.edits.get(&result.id).is_none_or(|edit| !edit.protects_result())
        }).min_by_key(|(_, result)| {
            let Some(path) = self.dock.find_tab(&Tab::Generated(result.id)) else {
                return 0;
            };
            if focused == Some(result.id) {
                return 3;
            }
            if self.dock.leaf(path.node_path()).is_ok_and(|leaf| leaf.active == path.tab) {
                return 2;
            }
            1
        }).map(|(index, _)| index)
    }

    pub(super) fn visible_generated_tab(&self) -> Option<u64> {
        let generated_at = |path| {
            let leaf = self.dock.leaf(path).ok()?;
            match leaf.tabs.get(leaf.active.0) {
                Some(Tab::Generated(id)) => Some(Some(*id)),
                Some(Tab::Home) => Some(None),
                _ => None,
            }
        };
        self.dock.focused_leaf().and_then(generated_at).or_else(|| {
            generated_at(self.map_tab_path()?.node_path())
        }).flatten()
    }

    pub(super) fn generated_view_controls(&mut self, ui: &mut egui::Ui) -> bool {
        let Some(id) = self.visible_generated_tab() else {
            return false;
        };
        let Some(map) = self.generation.results.iter_mut().find(|map| map.id == id) else {
            return false;
        };
        let available_3d = map.archive.as_ref().is_some_and(|archive| archive.image.is_some()) && map.overview.is_some();
        let scene = self.seed_scenes.entry(id).or_default();
        for (enabled, label) in [(false, "2D map"), (true, "3D terrain")] {
            if ui.add_enabled(!enabled || available_3d,
                egui::Button::selectable(scene.camera.enabled == enabled, label)).clicked()
            {
                scene.camera.enabled = enabled;
                ui.close();
            }
        }
        ui.separator();
        if ui.button("Fit map").clicked() {
            map.fit_map();
            scene.camera.reset();
            ui.close();
        }
        ui.separator();
        ui.checkbox(&mut map.grid, "Grid");
        ui.checkbox(&mut map.no_build, "No-build zones");
        true
    }

    pub(super) fn focused_map_edit(&self, ctx: &egui::Context) -> Option<u64> {
        if let Some(id) = self.focused_map_canvas(ctx) {
            return Some(id);
        }
        let Some(leaf) = self.dock.focused_leaf() else {
            return self.visible_generated_tab();
        };
        let leaf = self.dock.leaf(leaf).ok()?;
        match leaf.tabs.get(leaf.active.0)? {
            Tab::Generated(id) => Some(*id),
            _ => None,
        }
    }

    pub(super) fn can_step_map_edit(&self, id: u64, redo: bool) -> bool {
        self.generation.edits.get(&id).is_some_and(|edit| edit.can_step(redo))
    }

    pub(super) fn focused_map_canvas(&self, ctx: &egui::Context) -> Option<u64> {
        let focused = ctx.memory(|memory| memory.focused())?;
        self.generation.edits.iter().find_map(|(&id, edit)| {
            if edit.canvas != Some(focused) {
                return None;
            }
            let path = self.dock.find_tab(&Tab::Generated(id))?;
            let leaf = self.dock.leaf(path.node_path()).ok()?;
            (leaf.active == path.tab).then_some(id)
        })
    }

    pub(super) fn step_map_edit(&mut self, id: u64, redo: bool) {
        let Some(map) = self.generation.results.iter().find(|result| result.id == id) else {
            return;
        };
        let Some(edit) = self.generation.edits.get_mut(&id) else {
            return;
        };
        match edit.step(redo, map) {
            Ok(true) => self.generation.revision += 1,
            Ok(false) => {}
            Err(error) => self.status = error,
        }
    }
    pub(super) fn generated_summary(&mut self, ui: &mut egui::Ui, id: u64) {
        let can_enqueue = self.generation.can_enqueue(1);
        let Some(result) = self.generation.results.iter().find(|r| r.id == id) else {
            return;
        };
        let mut apply = false;
        let mut showing_monument = false;
        if let Some(edit) = self.generation.edits.get_mut(&id) {
            if let Some((index, monument)) = edit.selected_monument
                .filter(|&index| !edit.monuments.excluded(index))
                .and_then(|index| result.monuments.get(index).map(|monument| (index, monument)))
            {
                showing_monument = true;
                ui.add(egui::Label::new(egui::RichText::new(&monument.name).size(16.).strong()).truncate())
                    .on_hover_text(&monument.name);
                let accepted = [monument.position[0], monument.position[2], monument.yaw];
                let before = edit.poses.get(&index).copied();
                let mut pose = before.unwrap_or(accepted);
                let half = result.recipe.size as f32 / 2.;
                let can_move = edit.job.is_none() && !edit.dragging()
                    && result.missing == 0 && result.archive_issue.is_empty()
                    && matches!(result.image, Some(Ok(_)))
                    && movable_monument(&result.recipe, monument);
                ui.add_enabled_ui(can_move, |ui| {
                    egui::Grid::new(("generated-placement", id, index)).num_columns(2).show(ui, |ui| {
                        for (axis, label) in ["Preferred X", "Preferred Z", "Rotation"].into_iter().enumerate() {
                            let name = ui.label(label);
                            let field = if axis == 2 {
                                egui::DragValue::new(&mut pose[axis]).range(0. ..=360.).suffix("°")
                            } else {
                                egui::DragValue::new(&mut pose[axis]).range(-half..=half).suffix(" m")
                            };
                            ui.add(field).labelled_by(name.id);
                            ui.end_row();
                        }
                    });
                    let mut limit = edit.move_limit(result.recipe.size);
                    ui.horizontal(|ui| {
                        let label = ui.label("Max move");
                        if ui.add(egui::DragValue::new(&mut limit).range(1..=result.recipe.size).suffix(" m"))
                            .labelled_by(label.id).changed()
                        {
                            edit.move_limit_m = Some(limit);
                            edit.apply_error = None;
                            edit.last_issue = None;
                            self.generation.revision += 1;
                        }
                    });
                });
                if before.unwrap_or(accepted) != pose {
                    let after = (pose != accepted).then_some(pose);
                    if let Some(pose) = after { edit.poses.insert(index, pose); }
                    else { edit.poses.remove(&index); }
                    edit.record(DraftChange::Monument { index, before, after });
                    self.generation.revision += 1;
                }
                if let Some(prefab) = &monument.prefab {
                    ui.collapsing("Monument details", |ui| { ui.small(prefab); });
                }
                ui.separator();
            }
            if edit.job.is_some() {
                ui.weak("Rebuild queued or running");
            } else if edit.has_changes() {
                let ready = can_enqueue && !edit.dragging();
                apply = ui.add_enabled(ready, egui::Button::new(
                    egui::RichText::new("Apply changes").color(Color32::WHITE),
                ).fill(workbench_style::PRIMARY_ACTION)).clicked();
            }
        }
        ui.weak(format!("{} m · {} monuments", result.recipe.size, result.placed));
        if !showing_monument && !result.monuments.is_empty() {
            ui.weak("Select a placed monument to edit its position.");
        }
        if result.missing > 0 {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("{} required sites missing", result.missing),
            );
        }
        if let Some(policy) = result.recipe.roads.as_ref().and_then(|roads| roads.staged.as_ref()) {
            let missing = policy.roadside_preferences.iter().filter(|saved| {
                saved.presence != Some(Presence::Excluded) && !result.monuments.iter().any(|marker| {
                    marker.id == saved.request && marker.prefab.as_deref() == Some(&saved.prefab)
                })
            }).count();
            if missing > 0 {
                let message = if missing == 1 {
                    "1 roadside monument could not be placed".into()
                } else {
                    format!("{missing} roadside monuments could not be placed")
                };
                ui.colored_label(ui.visuals().warn_fg_color, message);
            }
        }
        if !result.archive_issue.is_empty() {
            ui.colored_label(ui.visuals().warn_fg_color,"Map export unavailable")
                .on_hover_ui(|ui| { ui.label(diagnostic_ui::summary(&result.archive_issue).as_ref()); });
        }
        if apply {
            self.apply_map_edit(id, ui.ctx());
        }
    }
    pub(super) fn map_tabs_menu(&mut self, ui: &mut egui::Ui) {
        let mut open = None;
        if ui.button("Home").clicked() {
            open = Some(Tab::Home);
        }
        for result in &self.generation.results {
            if ui
                .button(&result.label)
                .on_hover_ui(|ui| { ui.label(format!("{} m", result.recipe.size)); })
                .clicked()
            {
                open = Some(Tab::Generated(result.id));
            }
        }
        if let Some(tab) = open {
            ui.close();
            self.show_tab(tab);
        }
    }
    pub(super) fn generated_title(&self, id: u64) -> String {
        self.generation
            .results
            .iter()
            .find(|r| r.id == id)
            .map(|r| r.label.as_str())
            .or_else(|| self.generation.queue.iter().find(|job| job.id == id).map(|job| job.label.as_str()))
            .unwrap_or("Map unavailable").to_owned()
    }
    fn queue_actions(&mut self, ui: &mut egui::Ui) -> egui::Response {
        let running = self.generation.busy() || !self.generation.execution.jobs.is_empty();
        let config = MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
        let content = egui::Atoms::new((
            "Actions", egui::Atom::custom(egui::Id::new("queue-actions-chevron"), Vec2::splat(10.)),
        ));
        MenuButton::from_button(egui::Button::new(content).corner_radius(6.))
            .config(config).ui(ui, |ui| {
            ui.set_max_width(560.);
            if self.generation_in_progress() {
                let (label, help) = if running {
                    ("Cancel all", "Stop all running maps and cancel all waiting maps.")
                } else {
                    ("Cancel preview", "Stop the current recipe preview.")
                };
                if ui.button(label).on_hover_text(help).clicked() {
                    self.cancel_generation();
                    self.status = if running { "Generation cancelled." } else { "Preview cancelled." }.into();
                    ui.close();
                    return;
                }
            }
            if running && ui.button("Locate running map").clicked() {
                self.locate_running_map(ui.ctx());
                ui.close();
            }
            if let Some(path) = failed_recipe_path(self.settings_path.as_deref())
                && path.is_file()
                && ui.button("Save last failed recipe…").clicked()
            {
                ui.close();
                match mapper_studio::profile::Profile::load_snapshot(&path) {
                    Ok(profile) => {
                        if let Some(status) =
                            save_result_recipe(&profile.name, &profile.recipe, None, false)
                        {
                            self.status = status;
                        }
                    }
                    Err(error) => {
                        self.status = format!("Could not read the last failed recipe. {error}");
                    }
                }
            }
            let summary = diagnostic_ui::summary(&self.status);
            if !summary.is_empty() {
                ui.label(summary.as_ref());
            }
            if summary.as_ref() != self.status {
                diagnostic_ui::copy_details(ui, &self.status);
                ui.collapsing("Technical report", |ui| {
                    diagnostic_ui::report_preview(ui, &self.status);
                });
            }
            if let Some(preview) = &self.preview {
                let metrics = &preview.metrics;
                ui.collapsing("Preview timings", |ui| {
                    ui.label(format!(
                        "Total {:.1} ms · stage images {} · drawing {:.1} ms",
                        metrics.elapsed_ms, metrics.snapshots, metrics.snapshot_ms
                    ));
                    ui.label(format!(
                        "Ocean {:.1} ms · native source {}",
                        metrics.ocean_ms,
                        if metrics.reused_native_source { "reused" } else { "generated" }
                    ));
                    ui.label(&metrics.terrain_cache);
                });
            }
            let history = self.generation.history_len();
            let waiting = self.generation.waiting_count();
            if waiting > 0 || history > 0 {
                ui.separator();
            }
            if waiting > 0 && ui.button(format!("Remove waiting maps ({waiting})"))
                .on_hover_text("Remove waiting jobs and close their tabs. The running map will finish.")
                .clicked()
            {
                self.generation.remove_waiting_maps();
                ui.close();
            }
            if history > 0 {
                if ui.button("Clear queue history")
                    .on_hover_text("Remove cards from previous runs and keep their map tabs.")
                    .clicked()
                {
                    self.generation.discard_queue_prefix(history);
                    self.generation.revision += 1;
                    ui.close();
                }
            }
        }).0
    }
    fn can_load_queue_recipe(&self) -> bool {
        (self.generation.busy() || !self.generation_in_progress())
            && !self.project_io.busy()
            && self.drag_before.is_none()
            && self.pending.is_none()
    }

    fn can_requeue(&self) -> bool {
        self.can_load_queue_recipe() && self.generation.can_enqueue(1)
    }

    fn requeue_job(&mut self, id: u64, ctx: &egui::Context) {
        if !self.can_requeue() {
            return;
        }
        let Some(item) = self.generation.queue.iter().find(|item| {
            item.id == id && matches!(item.state, State::Failed | State::Cancelled)
        }) else {
            return;
        };
        let cancelled_label = (item.state == State::Cancelled).then(|| item.label.clone());
        let plan = BatchPlan::exact(item.recipe.clone());
        let origin = item.origin.clone();
        if mapper_studio::native_preview::is_native(&plan.recipe.preview_engine)
            && !self.source.inspecting()
            && self.source.bound_root_ref(&plan.recipe).is_none()
        {
            self.open_installation_settings();
            self.status = GenerationBlocker::MatchingInstallation.message().into();
            return;
        }
        let new_id = match self.enqueue_generation_from(plan, ctx, true, origin) {
            Ok(new_id) => new_id,
            Err(error) => {
                self.status = error;
                return;
            }
        };
        if let Some(label) = cancelled_label {
            if let Some(index) = self.generation.queue.iter().position(|item| item.id == id) {
                self.generation.queue.remove(index);
                if self.generation.queue_focus == Some(id) {
                    self.generation.queue_focus = None;
                }
                if let Some(run) = &mut self.generation.run {
                    run.remove_earlier_item(index);
                }
                self.generation.retired_tabs.push(id);
                self.generation.closed_job_tabs.remove(&id);
                self.generation.elapsed.remove(&id);
            }
            if let Some(item) = self.generation.queue.iter_mut().find(|item| item.id == new_id) {
                item.label = label;
            }
        }
        self.generation.pending_tabs.push(new_id);
        self.show_tab(Tab::Generated(new_id));
        ctx.request_repaint();
    }

    fn load_queue_recipe(&mut self, id: u64) {
        if !self.can_load_queue_recipe() {
            return;
        }
        let Some(item) = self.generation.queue.iter().find(|item| {
            item.id == id && matches!(item.state, State::Failed | State::Incomplete)
        }) else {
            return;
        };
        self.generation.recipe_recovery = diagnostic_ui::recipe_section(&item.detail);
        if self.generation.busy() {
            let recipe = item.recipe.clone();
            self.with_recipe_draft(|studio| {
                if studio.project != recipe {
                    studio.project = recipe;
                    studio.selected = None;
                }
            });
        } else if self.project != item.recipe {
            let previous = std::mem::replace(&mut self.project, item.recipe.clone());
            self.history.record(previous, &self.project);
            self.skip_history = true;
            self.selected = None;
            self.drag_splat = None;
        }
        self.show_tab(Tab::Recipe(self.recipe_tabs.active()));
        self.status = "Recipe loaded.".into();
    }

    fn locate_running_map(&mut self, ctx: &egui::Context) {
        if !self.generation.queue_filter.includes(State::Running) {
            self.generation.queue_filter = QueueFilter::All;
        }
        self.generation.revealed_job = None;
        self.generation.queue_focus = self.generation.queue.iter().find(|job| job.state == State::Running).map(|job| job.id);
        ctx.request_repaint();
    }

    fn apply_queue_action(&mut self, action: Option<QueueAction>, ctx: &egui::Context) {
        match action {
            Some(QueueAction::OpenMap(id)) => self.show_tab(Tab::Generated(id)),
            Some(QueueAction::UseSeed(seed)) => self.use_queue_seed(seed),
            Some(QueueAction::LoadRecipe(id)) => self.load_queue_recipe(id),
            Some(QueueAction::CancelMap(id)) => self.cancel_queued_map(id, ctx),
            Some(QueueAction::StartMap(id)) => self.request_queue_job(id, ctx),
            Some(QueueAction::StartAll) => {
                self.generation.execution.drain_parallel = true;
                ctx.request_repaint();
            }
            Some(QueueAction::TogglePause(id)) => self.toggle_queue_job_pause(id, ctx),
            Some(QueueAction::ToggleQueuePause) => {
                self.generation.queue_paused ^= true;
                ctx.request_repaint();
            }
            Some(QueueAction::Requeue(id)) => self.requeue_job(id, ctx),
            Some(QueueAction::RunNext(id)) => self.run_queued_map_next(id, ctx),
            Some(QueueAction::MoveWaiting { id, target }) => self.move_waiting_map(id, target, ctx),
            Some(QueueAction::StepWaiting { id, direction }) => self.step_waiting_map(id, direction, ctx),
            Some(QueueAction::SaveRecipe(id)) => {
                if let Some(item) = self.generation.queue.iter().find(|item| item.id == id)
                    && let Some(status) = save_result_recipe(&item.label, &item.recipe, Some(id), false)
                {
                    self.status = status;
                }
            }
            None => {}
        }
    }
}

impl Generation {
    pub(super) fn edited_stamp_paths(&self, id: u64) -> impl Iterator<Item = &str> {
        let result = self.results.iter().find(|result| result.id == id);
        self.edits.get(&id).into_iter().flat_map(move |edit| {
            edit.drag.iter().map(|drag| drag.index).chain(edit.poses.keys().copied())
                .filter_map(move |index| result?.monuments.get(index)?.prefab.as_deref())
                .chain(edit.monuments.additions.iter().map(|addition| addition.prefab.as_str()))
        })
    }
}

impl Studio {
    pub(super) fn generated_map(&mut self, ui: &mut egui::Ui, id: u64) {
        if !self.generation.results.iter().any(|result| result.id == id) {
            self.pending_map(ui, id);
            return;
        }
        self.generation.prepare_preview(id, ui.ctx());
        let can_enqueue = self.generation.can_enqueue(1) && self.drag_before.is_none();
        let active_job = self.generation.run.as_ref().map(|run| run.active_id);
        let incomplete_preview = self.generation.edits.get(&id)
            .and_then(|edit| edit.last_issue.as_ref())
            .is_some_and(|issue| issue.incomplete
                && self.generation.results.iter().any(|map| map.id == issue.id));
        let Some(result) = self.generation.results.iter_mut().find(|r| r.id == id) else {
            ui.label("This map is no longer in the workspace.");
            return;
        };
        let scene_3d = result.archive.as_ref().and_then(|archive| archive.image.as_ref())
            .cloned().zip(result.overview.clone());
        let scene = self.seed_scenes.entry(id).or_default();
        if scene_3d.is_none() { scene.camera.enabled = false; }
        let edit = self.generation.edits.entry(id).or_default();
        let can_move = result.missing == 0 && result.archive_issue.is_empty()
            && edit.job.is_none() && matches!(result.image, Some(Ok(_)));
        let mut action = None;
        let mut new_move_limit = None;
        result.restrictions.sync(&result.recipe, &self.restrictions, &mut result.monuments);
        let compact = ui.available_width() < 572.;
        let has_moves = edit.staged_poses().next().is_some();
        let has_draft = !edit.paths.changes().is_empty() || has_moves
            || !edit.monuments.is_empty();
        let apply_error = edit.apply_error.as_deref().filter(|_| has_draft);
        let issue = edit.last_issue.as_ref().filter(|_| has_draft && apply_error.is_none());
        let issue_detail = apply_error.or_else(|| issue.map(|item| item.detail.as_str()));
        let has_issue = issue_detail.is_some() || !result.archive_issue.is_empty();
        let has_action = edit.job.is_some() || has_draft;
        let narrow_issue = ui.available_width() < 300.;
        let fit_in_menu = ui.available_width() < if has_issue && has_action { 315. } else { 265. };
        let viewport = ui.available_rect_before_wrap().intersect(ui.clip_rect());
        let floating = viewport.width() >= 680.;
        let toolbar = |ui: &mut egui::Ui| {
            workbench_style::map_glass_controls(ui);
            workbench_style::map_glass().show(ui, |ui| ui.horizontal(|ui| {
            seed_scene::view_toggle(ui, &mut scene.camera.enabled, scene_3d.is_some());
            ui.separator();
            if !fit_in_menu && ui.button("Fit").on_hover_text("Fit the whole map in the viewport.").clicked() {
                result.fit_map();
                scene.camera.reset();
            }
            if !compact && !scene.camera.enabled {
                ui.toggle_value(&mut result.grid, "Grid");
                ui.toggle_value(&mut result.no_build, "No-build").on_hover_text("Show no-build zones.");
            }
            ui.menu_button("View", |ui| {
                if fit_in_menu && ui.button("Fit map").clicked() {
                    result.fit_map();
                    scene.camera.reset();
                    ui.close();
                }
                if compact && !scene.camera.enabled {
                    ui.checkbox(&mut result.grid, "Grid");
                    ui.checkbox(&mut result.no_build, "No-build zones");
                    ui.separator();
                }
                if map_label_panel::controls(ui, &mut self.preferences.monument_markers)
                    && let Some(path) = &self.settings_path {
                    self.settings_message = self.preferences.save(path).err().unwrap_or_default();
                }
                ui.separator();
                ui.menu_button("Controls", |ui| {
                    ui.label("Drag empty space to pan · Scroll to zoom");
                    if can_move {
                        ui.label("Drag a monument to move it · Scroll while dragging to rotate");
                        ui.label("Click a road, trail, or surface rail to edit its spline");
                        ui.label("Drag the selected path or a handle to bend it · Double-click to add a point");
                        ui.label("Right-click for point actions · Delete removes the selected point");
                    }
                });
            });
            let map_label = if has_issue {
                let label = if narrow_issue {
                    "Map issue"
                } else if issue_detail.is_none() {
                    "Export blocked"
                } else if apply_error.is_some() {
                    "Rebuild blocked"
                } else if issue.is_some_and(|item| !item.incomplete) {
                    "Rebuild failed"
                } else {
                    "Rebuild incomplete"
                };
                egui::RichText::new(label).color(ui.visuals().warn_fg_color)
            } else {
                "Map".into()
            };
            let menu_config = MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
            MenuButton::new(map_label).config(menu_config).ui(ui, |ui| {
                if let Some(job) = edit.job {
                    if ui.button("Cancel rebuild").on_hover_text("Keep your edits as a draft.").clicked() {
                        ui.close();
                        action = Some(MapEditAction::CancelRebuild(job));
                    }
                    ui.separator();
                }
                if let Some(detail) = issue_detail {
                    ui.set_max_width(320.);
                    let heading = if apply_error.is_some() {
                        "Rebuild could not start"
                    } else if issue.is_some_and(|item| item.incomplete) {
                        "Last rebuild incomplete"
                    } else {
                        "Last rebuild failed"
                    };
                    ui.strong(heading);
                    let summary = diagnostic_ui::summary(detail);
                    ui.label(summary.as_ref());
                    if !detail.is_empty() {
                        diagnostic_ui::copy_error(ui, detail, Some(&result.recipe));
                        diagnostic_ui::copy_details(ui, detail);
                        if summary.as_ref() != detail {
                            ui.collapsing("Technical report", |ui| diagnostic_ui::report_preview(ui, detail));
                        }
                    }
                    if incomplete_preview && let Some(item) = issue
                        && ui.button("Open incomplete map").clicked()
                    {
                        ui.close();
                        action = Some(MapEditAction::OpenIncomplete(item.id));
                    }
                    ui.separator();
                }
                if !result.archive_issue.is_empty() {
                    ui.set_max_width(320.);
                    let detail = &result.archive_issue;
                    let summary = diagnostic_ui::summary(detail);
                    ui.label(summary.as_ref());
                    diagnostic_ui::copy_error(ui, detail, Some(&result.recipe));
                    if summary.as_ref() != detail {
                        ui.collapsing("Technical report", |ui| diagnostic_ui::details(ui, detail));
                    }
                    ui.separator();
                }
                ui.label(format!("{} m · {} monuments", result.recipe.size, result.placed));
                if can_move && has_moves && !edit.dragging() {
                    ui.horizontal(|ui| {
                        let label = ui.label("Monument move limit")
                            .on_hover_text("Maximum distance from each dropped position. If no site fits, the rebuild keeps the previous map.");
                        let mut limit = edit.move_limit(result.recipe.size);
                        if ui.add(egui::DragValue::new(&mut limit).range(1..=result.recipe.size).suffix(" m"))
                            .labelled_by(label.id).changed()
                        {
                            new_move_limit = Some(limit);
                        }
                    });
                }
                if let Some(elapsed) = self.generation.elapsed.get(&id) {
                    ui.label(format!("Generated in {}", elapsed_time(*elapsed)));
                }
                if compact {
                    map_seed_menu(ui, &result.recipe, true);
                }
                ui.separator();
                if edit.has_changes() && edit.job.is_none()
                    && ui.add_enabled(!edit.dragging(), egui::Button::new("Discard changes")).clicked()
                {
                    ui.close();
                    action = Some(MapEditAction::Discard);
                }
                let exporting = self.map_archive.exporting();
                let applied = edit.has_changes();
                let label = map_archive::Notification::export_label(result.recipe.options.webhook_enabled, applied);
                let mut export = ui.add_enabled(!exporting && result.exportable_archive().is_some(), egui::Button::new(label));
                if exporting {
                    export = export.on_disabled_hover_text("A map export is in progress");
                } else if result.archive_issue.is_empty() {
                    export = export.on_disabled_hover_text("This preview has no saved game map");
                }
                if export.clicked() && let Some(archive) = result.exportable_archive() {
                    ui.close();
                    let filename = result.recipe.options.filename(result.recipe.size, result.recipe.master_seed, &result.recipe.seeds, &result.label);
                    let directory = export_panel::dialog_directory(
                        self.preferences.export_folder.as_deref(),
                        self.path.as_deref(),
                    );
                    self.map_archive.export(
                        archive.clone(),
                        &filename,
                        map_archive::Notification::from_recipe(&result.recipe),
                        directory,
                        ui.ctx(),
                    );
                }
                let image_label = if applied { "Export applied image…" } else { "Export image…" };
                if ui.button(image_label).clicked() {
                    ui.close();
                    self.image_export_target = Some(id);
                    self.export_options_open = true;
                }
                let recipe_label = if applied { "Save applied recipe…" } else { "Save recipe…" };
                if ui.button(recipe_label).clicked() {
                    ui.close();
                    if let Some(status) = save_result_recipe(&result.label, &result.recipe, Some(id), true) {
                        self.status = status;
                    }
                }
            });
            if !compact {
                map_seed_menu(ui, &result.recipe, false);
                if ui.button(format!("{:.0}%", result.zoom * 100.))
                    .on_hover_text("Reset zoom and fit the map.").clicked()
                {
                    result.fit_map();
                }
            }
            let mut edit_action = |ui: &mut egui::Ui| {
                if !has_action {
                    return;
                }
                if let Some(job) = edit.job {
                    let status = if active_job == Some(job) { "Applying changes…" } else { "Changes queued" };
                    ui.weak(status).on_hover_text("See progress in Queue or cancel the rebuild in Map.");
                } else if has_draft {
                    let apply = !edit.dragging() && can_enqueue;
                    let label = egui::RichText::new("Apply changes").strong();
                    let button = if apply {
                        egui::Button::new(label.color(Color32::WHITE)).fill(workbench_style::PRIMARY_ACTION)
                    } else {
                        egui::Button::new(label)
                    };
                    if ui.add_enabled(apply, button)
                        .on_hover_text("Commit the pending edits. Dashed lines show proposed paths.")
                        .on_disabled_hover_text(if can_enqueue { "Finish the current drag." } else { GenerationBlocker::QueueFull.message() })
                        .clicked()
                    {
                        action = Some(MapEditAction::Apply);
                    }
                }
            };
            if floating {
                edit_action(ui);
            } else {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), edit_action);
            }
        }));
        };
        if floating {
            egui::Area::new(ui.id().with(("generated-toolbar", id)))
                .pivot(Align2::CENTER_TOP)
                .fixed_pos(egui::pos2(viewport.center().x, viewport.top() + 8.))
                .default_size(Vec2::ZERO)
                .constrain_to(viewport)
                .fade_in(false)
                .show(ui.ctx(), toolbar);
            let badge_y = if viewport.width() >= 780. && !has_action { 8. } else { 50. };
            egui::Area::new(ui.id().with(("generated-identity", id)))
                .fixed_pos(viewport.left_top() + egui::vec2(8., badge_y))
                .default_size(Vec2::ZERO)
                .constrain_to(viewport)
                .fade_in(false)
                .show(ui.ctx(), |ui| {
                    workbench_style::map_glass_controls(ui);
                    workbench_style::map_glass().show(ui, |ui| ui.horizontal(|ui| {
                        ui.strong(format!("{} m", result.recipe.size));
                        ui.separator();
                        ui.label(format!("Seed {}", result.recipe.terrain_key().seed));
                    })).response.on_hover_text("Map size and terrain seed");
                });
        } else {
            ui.scope(toolbar);
        }
        if let Some(limit) = new_move_limit {
            edit.move_limit_m = Some(limit);
            edit.last_issue = None;
            edit.apply_error = None;
            self.generation.revision += 1;
        }
        if scene.camera.enabled && let Some((snapshot, overview)) = scene_3d {
            edit.cancel_drag();
            edit.paths.cancel_gesture();
            edit.canvas = None;
            let source = self.show_monuments.then(|| self.monuments.model_source()).flatten()
                .filter(|_| self.project.installation == result.recipe.installation);
            let models = source.into_iter().flat_map(|source| {
                result.monuments.iter().filter_map(move |monument| {
                    let prefab = source.prefabs.get(monument.prefab.as_deref()?)?;
                    (!prefab.model.triangles.is_empty()).then(|| terrain_3d::ModelInstance::yaw(
                        &prefab.model, monument.position, monument.yaw,
                    ))
                })
            }).collect::<Vec<_>>();
            scene.draw(ui, result.recipe.size, snapshot, overview, &models);
            return;
        }
        let (outer, response) = ui.allocate_exact_size(
            ui.available_size().max(Vec2::splat(1.)),
            Sense::click_and_drag(),
        );
        edit.canvas = Some(response.id);
        let available = outer.width().min(outer.height());
        let fit = map_monuments::fit_scale(result.recipe.size, &result.monuments, available);
        let map_rect = |zoom, pan| Rect::from_center_size(
            outer.center() + pan, Vec2::splat(available * zoom * fit),
        );
        let mut owns_primary = edit.press.is_some() || edit.drag.is_some();
        let monument_menu = response.id.with("monument-actions");
        if can_move {
            let map = map_rect(result.zoom, result.pan);
            let pointer = MonumentPointer::read(ui);
            capture_monument_press(ui, &response, map, result,
                &self.prefabs, edit, &pointer);
            // Give monument centers priority over spline handles on the press
            // frame. Away from the center, a click can still select a route
            // over a footprint and a drag can still move that footprint.
            let monument_press = if edit.drag.is_some() || edit.press.is_some_and(|pick| {
                edit_screen(map, result.recipe.size, pick.origin[0], pick.origin[1])
                    .distance(pick.press) <= MONUMENT_PICK_RADIUS
            }) {
                path_editor::MonumentPress::Marker
            } else if edit.press.is_some() {
                path_editor::MonumentPress::Footprint
            } else {
                path_editor::MonumentPress::None
            };
            let (path_owned, change) = edit.paths.input(
                ui, &response, &result.routes, map, result.recipe.size, monument_press,
            );
            match change {
                Ok(true) => {
                    if let Some(&change) = edit.paths.changes().last() {
                        edit.record(DraftChange::Spline(change));
                    }
                    self.generation.revision += 1;
                }
                Ok(false) => {}
                Err(error) => self.status = error,
            }
            if path_owned {
                if response.clicked() { edit.selected_monument = None; }
                edit.cancel_drag();
            } else {
                let clicked = edit.press.filter(|_| response.clicked() && edit.drag.is_none());
                if response.clicked() {
                    edit.selected_monument = clicked.map(|pick| pick.index);
                }
                let gesture = edit.drag.or(edit.press);
                if let Some(gesture) = gesture
                    && response.dragged_by(egui::PointerButton::Primary)
                {
                    edit.selected_monument = Some(gesture.index);
                }
                if edit_monument_input(ui, &response, map, &result.recipe, edit, &pointer) {
                    edit.selected_monument = gesture.map(|pick| pick.index);
                    self.generation.revision += 1;
                } else if let Some(pick) = clicked {
                    ui.ctx().data_mut(|data| data.insert_temp(monument_menu, pick.index));
                    egui::Popup::open_id(ui.ctx(), monument_menu);
                }
            }
            owns_primary |= path_owned || edit.press.is_some() || edit.drag.is_some();
        } else {
            edit.cancel_drag();
            edit.paths.cancel_gesture();
        }
        let delta = ui.input(|input| input.pointer.delta());
        if !owns_primary
            && (response.dragged_by(egui::PointerButton::Middle)
                || response.dragged_by(egui::PointerButton::Primary))
        {
            result.pan += delta;
        }
        if !owns_primary && response.hovered() {
            let line = ui.ctx().options(|options| options.input_options.line_scroll_speed);
            let scroll: f32 = ui.input(|input| input.raw.events.iter().filter_map(|event| match event {
                egui::Event::MouseWheel { unit, delta, .. } => Some(delta.y * match unit {
                    egui::MouseWheelUnit::Point => 1.,
                    egui::MouseWheelUnit::Line => line,
                    egui::MouseWheelUnit::Page => outer.height(),
                }),
                _ => None,
            }).sum());
            if scroll != 0. {
                let previous = result.zoom;
                result.zoom = (result.zoom * (scroll * 0.002).exp()).clamp(0.25, 8.);
                if let Some(pointer) = response.hover_pos() {
                    result.pan = pointer - outer.center()
                        - (pointer - outer.center() - result.pan) * (result.zoom / previous);
                }
            }
        }
        let map = map_rect(result.zoom, result.pan);
        let painter = ui.painter().with_clip_rect(outer);
        painter.rect_filled(outer, 0., result.background);
        match &result.image {
            Some(Ok(texture)) => painter.image(
                texture.id(),
                map,
                Rect::from_min_max(Pos2::ZERO, egui::pos2(1., 1.)),
                Color32::WHITE,
            ),
            state => {
                let text = match state {
                    Some(Err(_)) => "Map preview unavailable",
                    _ => "Loading map preview…",
                };
                painter.text(
                    outer.center(), Align2::CENTER_CENTER, text,
                    FontId::proportional(13.), Color32::WHITE,
                );
                if let Some(Err(error)) = state {
                    response.clone().on_hover_ui(|ui| { ui.label(diagnostic_ui::summary(error).as_ref()); });
                }
                self.map_edit_action(id, action, ui.ctx());
                return;
            }
        };
        if can_move
            && let Some(template) = response.dnd_hover_payload::<MonumentTemplate>()
            && template.map == id
            && let Some(pointer) = ui.input(|input| input.pointer.interact_pos())
            && map.contains(pointer)
        {
            let destination = result.monuments.iter().enumerate()
                .filter(|(index, monument)| !edit.monuments.excluded(*index)
                    && movable_monument(&result.recipe, monument))
                .map(|(index, monument)| {
                    let pose = edit.poses.get(&index).copied().unwrap_or([
                        monument.position[0], monument.position[2], monument.yaw,
                    ]);
                    let center = edit_screen(map, result.recipe.size, pose[0], pose[1]);
                    (pointer.distance_sq(center), index, center)
                })
                .filter(|(distance, _, _)| *distance <= MONUMENT_PICK_RADIUS.powi(2))
                .min_by(|a, b| a.0.total_cmp(&b.0));
            let center = destination.map_or(pointer, |(_, _, center)| center);
            painter.circle_stroke(center, 9., Stroke::new(1.5, Color32::from_rgb(255, 213, 138)));
            let label = if destination.is_some() { "Drop to replace" } else { "Drop to add" };
            marker_label(&painter, outer, center, label);
            if response.dnd_release_payload::<MonumentTemplate>().is_some() {
                let size = result.recipe.size as f32;
                let position = [
                    ((pointer.x - map.left()) / map.width() - 0.5) * size,
                    (0.5 - (pointer.y - map.top()) / map.height()) * size,
                ];
                match edit.place_template(result, &template, position, destination.map(|(_, index, _)| index)) {
                    Ok(changed) => self.generation.revision += u64::from(changed),
                    Err(error) => self.status = error,
                }
            }
        }
        for addition in &edit.monuments.additions {
            let point = edit_screen(map, result.recipe.size, addition.position[0], addition.position[1]);
            let color = Color32::from_rgb(255, 213, 138);
            painter.circle_stroke(point, 7., Stroke::new(1.5, color));
            painter.line_segment([point - Vec2::new(3., 0.), point + Vec2::new(3., 0.)], Stroke::new(1., color));
            painter.line_segment([point - Vec2::new(0., 3.), point + Vec2::new(0., 3.)], Stroke::new(1., color));
            if response.hover_pos().is_some_and(|pointer| pointer.distance(point) <= 9.) {
                marker_label(&painter, outer, point, &addition.name);
            }
        }
        if result.no_build {
            let (_,partial)=result.restrictions.paint_monuments(&painter,map,outer,result.recipe.size as f32,&result.monuments);
            if partial>0 {
                let status = if self.restrictions.pending_for(&result.recipe) {
                    "Loading no-build zones…"
                } else {
                    "Some no-build zones are unavailable"
                };
                painter.text(outer.left_bottom()+egui::vec2(8.,-8.),Align2::LEFT_BOTTOM,
                    status,FontId::proportional(12.),Color32::from_rgb(255,190,130));
            }
        }
        let grid_style = result.map_font.as_ref().and_then(|font| font.grid.as_deref());
        if result.grid {
            restriction_panel::grid(&painter, map, outer, result.recipe.size, grid_style);
        }
        let markers = self.preferences.monument_markers;
        let marker_position = |index, monument: &map_monuments::Monument| {
            edit.poses.get(&index).map_or_else(
                || edit_screen(map, result.recipe.size, monument.position[0], monument.position[2]),
                |pose| edit_screen(map, result.recipe.size, pose[0], pose[1]),
            )
        };
        let monument_hover = can_move && response.hover_pos().is_some_and(|pointer| {
            result.monuments.iter().enumerate().any(|(index, monument)| {
                !edit.monuments.excluded(index)
                    && pointer.distance_sq(marker_position(index, monument)) <= MONUMENT_PICK_RADIUS.powi(2)
                    && movable_monument(&result.recipe, monument)
            })
        });
        let path_hover = if edit.drag.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            None
        } else if monument_hover && !edit.paths.dragging() {
            response.clone().on_hover_cursor(egui::CursorIcon::Grab);
            None
        } else if can_move && edit.press.is_none() {
            edit.paths.hover(&result.routes, map, result.recipe.size, &response)
        } else {
            None
        };
        {
            let stamps = painter.with_clip_rect(map.intersect(outer));
            let moved = edit.poses.iter().filter_map(|(&index, &pose)| {
                if edit.monuments.excluded(index) { return None; }
                Some((result.monuments.get(index)?.prefab.as_deref()?, pose))
            });
            let additions = edit.monuments.additions.iter().map(|addition| {
                let [x, z] = addition.position;
                (addition.prefab.as_str(), [x, z, addition.yaw.unwrap_or(0.)])
            });
            for (path, pose) in moved.chain(additions) {
                let Some(stamp) = self.prefabs.stamp(&result.recipe, path) else { continue; };
                let points = stamp.geometry.quad(pose[0], pose[1], pose[2]);
                let mut mesh = egui::Mesh::with_texture(stamp.texture.id());
                for (coordinate, uv) in points.into_iter().zip([[0., 0.], [1., 0.], [1., 1.], [0., 1.]]) {
                    mesh.vertices.push(egui::epaint::Vertex {
                        pos: edit_screen(map, result.recipe.size, coordinate[0], coordinate[1]),
                        uv: egui::pos2(uv[0], uv[1]),
                        color: Color32::from_white_alpha(210),
                    });
                }
                mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
                stamps.add(egui::Shape::mesh(mesh));
            }
        }
        // The retained image already contains the accepted routes. During Apply,
        // keep that revision visible until its atomically rebuilt replacement arrives.
        if edit.job.is_none() {
            edit.paths.paint(&painter, &result.routes, map, result.recipe.size, path_hover, can_move);
        }
        let mut hovered: Option<(f32, Pos2, &map_monuments::Monument, bool)> = None;
        for (index, monument) in result.monuments.iter().enumerate() {
            if edit.monuments.excluded(index) { continue; }
            let moved = edit.poses.get(&index).copied();
            let path = monument.prefab.as_deref();
            if moved.is_none() && self.preferences.monument_marker_hidden(path) { continue; }
            let display_name = self.preferences.monument_label(path, &monument.name);
            let point = marker_position(index, monument);
            if markers != mapper_studio::map_labels::Mode::Text
                && !outer.expand(monument_icon_panel::ICON_SIDE).contains(point) { continue; }
            let hidden_text = markers == mapper_studio::map_labels::Mode::Text
                && !result.restrictions.shows_map_text(monument.prefab.as_deref());
            if hidden_text && moved.is_none() { continue; }
            if markers == mapper_studio::map_labels::Mode::Text && !hidden_text
                && !self.monument_icons.labels
                    .bounds(ui.ctx(), result.map_font.as_ref(), display_name)
                    .translate(point.to_vec2()).intersects(outer)
            {
                continue;
            }
            if markers != mapper_studio::map_labels::Mode::None || moved.is_some()
            {
                if hidden_text || markers == mapper_studio::map_labels::Mode::None
                    || !self.monument_icons.paint(&painter, point, monument.prefab.as_deref(),
                        &self.preferences, self.settings_path.as_deref(),
                        map_label_panel::Text { name: display_name, source: result.map_font.as_ref() })
                {
                    painter.circle_filled(point, monument_icon_panel::PLACED_RADIUS, if moved.is_some() {
                        Color32::from_rgb(255, 213, 138)
                    } else {
                        Color32::from_rgb(118, 207, 245)
                    });
                }
                if let Some(pointer) = response.hover_pos()
                    && markers != mapper_studio::map_labels::Mode::Text
                    && (can_move || markers == mapper_studio::map_labels::Mode::Icons)
                {
                    let distance = point.distance(pointer);
                    if distance <= MONUMENT_PICK_RADIUS && hovered.as_ref().is_none_or(|(previous, ..)| distance < *previous) {
                        hovered = Some((distance, point, monument, moved.is_some()));
                    }
                }
            }
        }
        if let Some((_, point, monument, moved)) = hovered {
            let name = self.preferences.monument_label(monument.prefab.as_deref(), &monument.name);
            let label = if moved {
                Cow::Owned(format!("{name} · Move preview"))
            } else {
                Cow::Borrowed(name)
            };
            marker_label(&painter, outer, point, &label);
        } else if result.grid && edit.drag.is_none() && !edit.paths.dragging()
            && path_hover.is_none()
            && let Some(pointer) = response.hover_pos()
            && let Some(cell) = restriction_panel::grid_coordinate(map, pointer, result.recipe.size, grid_style)
        {
            response.clone().on_hover_text(cell);
        }
        if can_move && egui::Popup::is_id_open(ui.ctx(), monument_menu) {
            let selected = ui.ctx().data(|data| data.get_temp::<usize>(monument_menu));
            if let Some((index, monument)) = selected.and_then(|index| {
                result.monuments.get(index).filter(|_| !edit.monuments.excluded(index))
                    .map(|monument| (index, monument))
            }) {
                let anchor = marker_position(index, monument) + Vec2::new(0., 10.);
                if outer.contains(anchor) {
                    egui::Popup::new(monument_menu, ui.ctx().clone(), anchor, ui.layer_id())
                        .open_memory(None)
                        .show(|ui| {
                            ui.strong(&monument.name);
                            if ui.button("Replace…").clicked() {
                                action = Some(MapEditAction::ReplaceMonument(index));
                                ui.close();
                            }
                            if ui.button("Remove").clicked() {
                                action = Some(MapEditAction::RemoveMonument(index));
                                ui.close();
                            }
                        });
                } else {
                    egui::Popup::close_id(ui.ctx(), monument_menu);
                }
            }
        } else if !can_move {
            egui::Popup::close_id(ui.ctx(), monument_menu);
        }
        self.map_edit_action(id, action, ui.ctx());
    }

    fn map_edit_action(&mut self, id: u64, action: Option<MapEditAction>, ctx: &egui::Context) {
        match action {
            Some(MapEditAction::Apply) => self.apply_map_edit(id, ctx),
            Some(MapEditAction::CancelRebuild(job)) => self.cancel_queued_map(job, ctx),
            Some(MapEditAction::OpenIncomplete(id)) => self.show_tab(Tab::Generated(id)),
            Some(MapEditAction::ReplaceMonument(index)) => self.choose_monument_replacement(ctx, id, index),
            Some(MapEditAction::RemoveMonument(index)) => {
                if let Err(error) = self.generation.set_monument_presence(id, index, Presence::Excluded) {
                    self.status = error;
                }
            }
            Some(MapEditAction::Discard) => {
                let discarded_changes = self.generation.edits.remove(&id)
                    .is_some_and(|edit| edit.has_changes());
                self.generation.revision += u64::from(discarded_changes);
            }
            None => {}
        }
    }
}

fn elapsed_time(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds >= 3600 {
        format!("{}:{:02}:{:02}", seconds / 3600, seconds / 60 % 60, seconds % 60)
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

enum MapEditAction {
    Apply,
    CancelRebuild(u64),
    Discard,
    OpenIncomplete(u64),
    ReplaceMonument(usize),
    RemoveMonument(usize),
}

fn target_index(recipe: &Project, monument: &map_monuments::Monument) -> Option<usize> {
    recipe.targets.iter().position(|target| {
        target.instance_id == monument.id && target.prefab_path == monument.prefab
            && target.presence != Presence::Excluded
    })
}

fn roadside_prefab<'a>(recipe: &Project, monument: &'a map_monuments::Monument) -> Option<&'a str> {
    let prefab = monument.prefab.as_deref()?;
    (recipe.performance.monuments
        && recipe.roads.as_ref().is_some_and(|roads| roads.staged.is_some())
        && mapper_studio::roadside_placement::is_editable_request_id(prefab, monument.id))
        .then_some(prefab)
}

fn movable_monument(recipe: &Project, monument: &map_monuments::Monument) -> bool {
    !monument.is_scenery_marker()
        && (target_index(recipe, monument).is_some() || roadside_prefab(recipe, monument).is_some())
}

fn edit_screen(map: Rect, size: u32, x: f32, z: f32) -> Pos2 {
    let size = size as f32;
    egui::pos2(map.left() + (x / size + 0.5) * map.width(), map.top() + (0.5 - z / size) * map.height())
}

struct MonumentPointer {
    press: Option<Pos2>,
    release: Option<Pos2>,
    down: bool,
    cancelled: bool,
}

impl MonumentPointer {
    fn read(ui: &egui::Ui) -> Self {
        use egui::PointerButton::Primary;
        ui.input(|input| {
            let mut pointer = Self {
                press: None,
                release: None,
                down: input.pointer.button_down(Primary),
                cancelled: !input.focused || input.key_pressed(egui::Key::Escape),
            };
            for event in &input.raw.events {
                match event {
                    egui::Event::PointerButton { pos, button: Primary, pressed, .. } => {
                        if *pressed {
                            pointer.press = Some(*pos);
                        } else {
                            pointer.release = Some(*pos);
                        }
                    }
                    egui::Event::PointerGone => pointer.cancelled = true,
                    _ => {}
                }
            }
            pointer
        })
    }
}

fn capture_monument_press(
    ui: &egui::Ui,
    response: &egui::Response,
    map: Rect,
    result: &Candidate,
    prefabs: &prefab_panel::PrefabPanel,
    edit: &mut MapEdit,
    pointer: &MonumentPointer,
) {
    if pointer.cancelled || (!pointer.down && pointer.release.is_none()) {
        edit.cancel_drag();
        return;
    }
    if edit.drag.is_none()
        && let Some(press) = pointer.press
    {
        edit.press = None;
        if response.interact_rect.contains(press)
            && ui.ctx().layer_id_at(press) == Some(ui.layer_id())
        {
            let recipe = &result.recipe;
            let size = recipe.size as f32;
            let world = [
                ((press.x - map.left()) / map.width() - 0.5) * size,
                (0.5 - (press.y - map.top()) / map.height()) * size,
            ];
            edit.press = result.monuments.iter().enumerate().filter_map(|(index, monument)| {
                if edit.monuments.excluded(index) || !movable_monument(recipe, monument) { return None; }
                let previous = edit.poses.get(&index).copied();
                let origin = previous.unwrap_or([monument.position[0], monument.position[2], monument.yaw]);
                let distance = edit_screen(map, recipe.size, origin[0], origin[1]).distance(press);
                let stamp_hit = || monument.prefab.as_deref()
                    .and_then(|path| prefabs.stamp(recipe, path))
                    .is_some_and(|stamp| stamp.hit_test_source(world, origin, false, false));
                (distance <= MONUMENT_PICK_RADIUS || (map.contains(press) && stamp_hit()))
                    .then_some((distance, MonumentDrag { index, press, origin, previous }))
            }).min_by(|a, b| a.0.total_cmp(&b.0)).map(|(_, drag)| drag);
        }
    }
}

fn edit_monument_input(
    ui: &egui::Ui,
    response: &egui::Response,
    map: Rect,
    recipe: &Project,
    edit: &mut MapEdit,
    pointer: &MonumentPointer,
) -> bool {
    use egui::PointerButton::Primary;
    let MonumentPointer { release, down, .. } = *pointer;
    if edit.drag.is_none()
        && let Some(pick) = edit.press
        && (response.drag_started_by(Primary) || release.is_some_and(|end| {
            end.distance(pick.press) > ui.ctx().options(|options| options.input_options.max_click_dist)
        }))
    {
        edit.drag = Some(pick);
        response.request_focus();
    }
    if let Some(drag) = edit.drag
        && let Some(pointer) = release.or_else(|| response.interact_pointer_pos()).or_else(|| {
            ui.input(|input| input.pointer.latest_pos())
        })
    {
        let scale = recipe.size as f32 / map.width().max(1.);
        let half = recipe.size as f32 * 0.5;
        let delta = (pointer - drag.press) * scale;
        let yaw = edit.poses.get(&drag.index).map_or(drag.origin[2], |pose| pose[2]);
        let rotation = ui.input(|input| {
            let scroll: f32 = input.raw.events.iter().filter_map(|event| match event {
                egui::Event::MouseWheel { delta, .. } => Some(delta.y),
                _ => None,
            }).sum();
            let step = if input.modifiers.shift { 1. } else { 15. };
            if scroll == 0. { 0. } else { scroll.signum() * step }
        });
        let pose = [(drag.origin[0] + delta.x).clamp(-half, half),
            (drag.origin[1] - delta.y).clamp(-half, half), (yaw + rotation).rem_euclid(360.)];
        if drag.previous.is_none() && pose == drag.origin {
            edit.poses.remove(&drag.index);
        } else {
            edit.poses.insert(drag.index, pose);
        }
    }
    if !down {
        edit.press = None;
        if let Some(drag) = edit.drag.take() {
            let after = edit.poses.get(&drag.index).copied();
            if after != drag.previous {
                edit.record(DraftChange::Monument {
                    index: drag.index,
                    before: drag.previous,
                    after,
                });
                return true;
            }
        }
    }
    false
}

fn failed_recipe_path(settings: Option<&Path>) -> Option<PathBuf> {
    settings.map(|path| path.with_file_name("last-failed.rec"))
}

fn retain_failed_recipe(settings: Option<&Path>, item: &QueueItem) {
    let Some(path) = failed_recipe_path(settings) else {
        return;
    };
    let saved = (|| {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        write_result_recipe(&path, &item.label, &item.recipe, false)
    })();
    if let Err(error) = saved {
        eprintln!("Could not retain failed generation recipe. {error}");
    }
}

fn save_result_recipe(
    label: &str,
    recipe: &Project,
    id: Option<u64>,
    embed_sources: bool,
) -> Option<String> {
    let seed = recipe.seeds[0].value;
    let name = id.map_or_else(
        || format!("seed-{seed}.rec"),
        |id| format!("seed-{seed}-map-{id}.rec"),
    );
    let path = rfd::FileDialog::new()
        .add_filter("Mapper Recipe", &["rec"])
        .set_file_name(name)
        .save_file()?;
    Some(match write_result_recipe(&path, label, recipe, embed_sources) {
        Ok(()) => "Recipe saved".into(),
        Err(error) => format!("Could not save recipe. {error}"),
    })
}

fn write_result_recipe(
    path: &Path,
    label: &str,
    recipe: &Project,
    embed_sources: bool,
) -> Result<(), String> {
    let profile = mapper_studio::profile::Profile::new(label.to_owned(), recipe.clone());
    if embed_sources {
        profile.save(path)
    } else {
        // The failed job may have been unable to read the source it pins.
        // Writing the queued recipe should work even in that case.
        profile.validate().and_then(|()| {
            mapper_studio::profile::write_document(path, &profile, true)
        })
    }
}

fn queue_recipe_button(ui: &mut egui::Ui, enabled: bool) -> egui::Response {
    ui.add_enabled(enabled, egui::Button::new("Edit recipe"))
        .on_hover_text("Load this map’s recipe into the Recipe panel. Undo restores your previous recipe.")
        .on_disabled_hover_text("Finish or cancel the current work first.")
}

fn queue_retry_button(ui: &mut egui::Ui, label: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(enabled, egui::Button::new(label).small())
        .on_hover_text("Generate another attempt with this map’s saved recipe and seeds.")
        .on_disabled_hover_text("Free a queue slot or finish the current operation.")
}

fn run_next_button(ui: &mut egui::Ui) -> egui::Response {
    ui.small_button("Run next")
        .on_hover_text("Move this map to the front of the waiting queue.")
}

fn seed_rows(recipe: &Project) -> impl Iterator<Item = (&'static str, u64)> + '_ {
    std::iter::once(("Master", u64::from(recipe.master_seed)))
        .chain(STAGES.into_iter().zip(&recipe.seeds).map(|(name, seed)| {
            (name, u64::from(seed.value))
        }))
        .chain(
            [
                ("/ring/seed", "Rail ring"),
                ("/approaches/seed", "Rail approaches"),
                ("/bed/branching/seed", "Rail branches"),
                ("/bed/siding/seed", "Rail sidings"),
            ]
            .into_iter()
            .filter_map(|(path, label)| {
                let seed = recipe.rails.as_ref()?.settings.pointer(path)?.as_u64()?;
                Some((label, seed))
            }),
        )
}

fn map_seed_menu(ui: &mut egui::Ui, recipe: &Project, submenu: bool) {
    let contents = |ui: &mut egui::Ui| {
        egui::Grid::new("generated-map-seeds")
            .num_columns(2)
            .spacing([18., 4.])
            .show(ui, |ui| {
                for (name, seed) in seed_rows(recipe) {
                    let label = ui.label(name);
                    ui.monospace(seed.to_string()).labelled_by(label.id);
                    ui.end_row();
                }
            });
        ui.separator();
        if ui.button("Copy seeds").clicked() {
            let text = seed_rows(recipe)
                .map(|(name, value)| format!("{name}: {value}"))
                .collect::<Vec<_>>()
                .join("\n");
            ui.ctx().copy_text(text);
            ui.close();
        }
    };
    if submenu {
        ui.menu_button("Seeds", contents);
    } else {
        egui::Popup::menu(&ui.button("Seeds"))
            .align(egui::RectAlign::BOTTOM_END)
            .show(contents);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn map_seed_menu_displays_and_copies_the_frozen_recipe_seeds() {
        for (dark, width) in [(true, 360), (false, 620)] {
            let ctx = egui::Context::default();
            ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
            workbench_style::install(&ctx);
            let mut recipe = Project::default();
            recipe.master_seed = u32::MAX;
            for (index, seed) in recipe.seeds.iter_mut().enumerate() {
                seed.value = 31001 + index as u32;
            }
            let height = 340;
            let textures = std::cell::RefCell::new(snapshot::Textures::default());
            let time = std::cell::Cell::new(0.);
            let render = |events| {
                time.set(time.get() + 0.15);
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(width as f32, height as f32))),
                        events,
                        focused: true,
                        time: Some(time.get()),
                        ..Default::default()
                    },
                    |ui| {
                        ui.horizontal(|ui| {
                            if width < 560 {
                                ui.menu_button("Map", |ui| map_seed_menu(ui, &recipe, true));
                            } else {
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    map_seed_menu(ui, &recipe, false);
                                });
                            }
                        });
                    },
                );
                textures.borrow_mut().update(&output.textures_delta);
                output.textures_delta.clear();
                output
            };
            let position = |output: &egui::FullOutput, name: &str| {
                output.shapes.iter().find_map(|shape| {
                    if let egui::Shape::Text(text) = &shape.shape
                        && text.galley.job.text == name
                    {
                        let bounds = text.galley.rect.translate(text.pos.to_vec2());
                        return shape.clip_rect.contains_rect(bounds).then(|| bounds.center());
                    }
                    None
                }).unwrap_or_else(|| panic!("Missing visible seed menu text: {name}"))
            };
            let click = |pos| {
                let event = |pressed| egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                };
                render(vec![egui::Event::PointerMoved(pos), event(true)]);
                render(vec![event(false)])
            };
            render(vec![]);
            if width < 560 {
                click(position(&render(vec![]), "Map"));
            }
            click(position(&render(vec![]), "Seeds"));
            render(vec![]);
            let menu = render(vec![]);
            if let Some(directory) = std::env::var_os("MAPPER_MAP_TABS_SNAPSHOTS") {
                let theme = if dark { "dark" } else { "light" };
                let path = PathBuf::from(directory).join(format!("map-seeds-{theme}-{width}.png"));
                let image = textures.borrow().raster(&ctx, menu.clone(), [width, height]);
                mapper_studio::image_export::write_png(&path, &image, || false).unwrap();
            }
            position(&menu, "Master");
            position(&menu, "4294967295");
            let copy = position(&menu, "Copy seeds");
            let output = click(copy);
            let copied = output.platform_output.commands.iter().find_map(|command| match command {
                egui::OutputCommand::CopyText(value) => Some(value.as_str()),
                _ => None,
            }).unwrap();
            let mut lines = copied.lines();
            assert_eq!(lines.next(), Some("Master: 4294967295"));
            for (index, stage) in STAGES.into_iter().enumerate() {
                assert_eq!(lines.next(), Some(format!("{stage}: {}", 31001 + index).as_str()));
            }
            assert_eq!(lines.next(), None);
        }
    }

    #[test]
    fn incomplete_maps_remain_inspectable_and_the_batch_continues() {
        let ctx = egui::Context::default();
        let mut app = super::super::tests::fixture(&ctx);
        app.project.size = 1000;
        app.project.targets.truncate(1);
        let target = &mut app.project.targets[0];
        target.x = 500.;
        target.z = 500.;
        target.radius = 1.;
        target.exact = true;
        target.presence = Presence::Required;
        let plan = BatchPlan {
            count: 3,
            nonce: 42,
            randomize: [true, false, false, false, false, false, false],
            ..BatchPlan::exact(app.project.clone())
        };
        let recipes = plan.recipes().unwrap();
        app.begin_generation(plan, &ctx);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        while app.generation.busy() && std::time::Instant::now() < deadline {
            app.poll(&ctx);
            app.poll_placement(&ctx);
            app.tick_generation(&ctx);
            app.apply_tab_request();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(!app.generation.busy(), "{}", app.status);
        assert_eq!(app.generation.results.len(), 3);
        assert!(
            app.generation
                .queue
                .iter()
                .all(|item| item.state == State::Incomplete
                    && item.detail.contains("1 required monument could not be placed")
                    && item.detail.contains(&item.recipe.targets[0].name))
        );
        assert_eq!(
            app.generation
                .results
                .iter()
                .map(|result| result.recipe.clone())
                .collect::<Vec<_>>(),
            recipes
        );
        assert!(
            app.generation
                .results
                .iter()
                .all(|result| result.exportable_archive().is_none() && result.missing == 1)
        );
        assert_eq!(app.status, "3 incomplete.");
        assert!(app.visible_generated_tab().is_some());
    }
    #[test]
    fn viewport_ocean_uses_installation_colors_and_restores_defaults() {
        let ctx=egui::Context::default();
        let mut app=color_fixture(&ctx,true);
        let recipe=app.project.clone();
        let (_,palette)=app.map_color_source().unwrap();
        let source=palette.defaults().unwrap();
        let color=|preferences:&mapper_studio::map_colors::Preferences| {
            let rgb=source.effective(preferences).water([0.;8],50.)
                .map(|v|v.round().clamp(0.,255.) as u8);
            Color32::from_rgb(rgb[0],rgb[1],rgb[2])
        };
        let original=app.viewport_background();
        assert_eq!(original,color(&Default::default()));
        app.preferences.map_colors.enabled=true;
        app.preferences.map_colors.overrides[10]=Some([15,120,180]);
        assert_eq!(app.viewport_background(),color(&app.preferences.map_colors));
        assert_ne!(app.viewport_background(),original);
        app.preferences.map_colors=Default::default();
        assert_eq!(app.viewport_background(),original);
        assert_eq!(app.project,recipe);
        assert_eq!(ocean_background(None,&Default::default()),MAP_BACKGROUND);
    }
    #[test]
    fn generated_map_overlay_controls_are_visible_and_leave_the_recipe_unchanged() {
        let ctx=egui::Context::default();
        let mut app=super::super::tests::fixture(&ctx);
        app.grid=false;app.show_no_build=false;
        let recipe=app.project.clone();
        app.generation.results.push(Candidate {
            identity:None,
            overview:None,
            archive:None,archive_issue:String::new(),
            id:77,label:"Map 77".into(),recipe:recipe.clone(),
            image:Some(Ok(ctx.load_texture("map-controls-test",egui::ColorImage::filled([8,8],Color32::GREEN),Default::default()))),
            zoom:1.,pan:Vec2::ZERO,placed:0,missing:0,monuments:Vec::new(),routes:Vec::new(),restrictions:Default::default(),
            grid:false,no_build:false,map_font:None,background:MAP_BACKGROUND,palette:None,
        });
        let screen=Rect::from_min_size(Pos2::ZERO,egui::vec2(1200.,700.));
        let render=|app:&mut Studio,events| {
            let mut frame = ctx.run_ui(egui::RawInput {
                screen_rect:Some(screen),events,focused:true,..Default::default()
            },|ui|app.generated_map(ui,77));
            frame.textures_delta.clear();
            frame
        };
        render(&mut app, Vec::new());
        for label in ["Grid","No-build"] {
            let output=render(&mut app,Vec::new());
            let point=output.shapes.iter().find_map(|s|match &s.shape {
                egui::Shape::Text(t) if t.galley.job.text==label=>Some(t.pos+t.galley.rect.center().to_vec2()),_=>None,
            }).expect("Map overlay control is visible");
            let button=|pressed|egui::Event::PointerButton {pos:point,button:egui::PointerButton::Primary,pressed,modifiers:Default::default()};
            let _=render(&mut app,vec![egui::Event::PointerMoved(point),button(true)]);
            let _=render(&mut app,vec![button(false)]);
        }
        let document=&app.generation.results[0];
        assert!(document.grid && document.no_build);
        assert_eq!(document.recipe,recipe);
        assert_eq!(app.project,recipe);
        assert!(!app.grid && !app.show_no_build);

        app.show_tab(Tab::Generated(77));
        app.apply_tab_request();
        app.generation.results[0].zoom = 2.;
        app.generation.results[0].pan = egui::vec2(16., 32.);
        let render = |app: &mut Studio, events| {
            let mut frame = ctx.run_ui(egui::RawInput {
                screen_rect: Some(screen), events, focused: true, ..Default::default()
            }, |ui| assert!(app.generated_view_controls(ui)));
            frame.textures_delta.clear();
            frame
        };
        for label in ["Fit map", "Grid"] {
            let output = render(&mut app, vec![]);
            let point = output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == label =>
                    Some(text.pos + text.galley.rect.center().to_vec2()),
                _ => None,
            }).unwrap();
            let button = |pressed| egui::Event::PointerButton {
                pos: point, button: egui::PointerButton::Primary, pressed,
                modifiers: Default::default(),
            };
            render(&mut app, vec![egui::Event::PointerMoved(point), button(true)]);
            render(&mut app, vec![button(false)]);
        }
        let result = &app.generation.results[0];
        assert_eq!((result.zoom, result.pan), (1., Vec2::ZERO));
        assert!(!result.grid && result.no_build);
        assert!(!app.grid && !app.show_no_build);
        assert_eq!(app.project, recipe);
    }
    #[test]
    fn generated_header_keeps_the_canvas_stable_and_exposes_map_actions_at_narrow_widths() {
        for dark in [false, true] {
            for width in [205., 280., 360., 520., 600., 1200.] {
                let ctx = egui::Context::default();
                ctx.set_theme(if dark { egui::Theme::Dark } else { egui::Theme::Light });
                workbench_style::install(&ctx);
                let mut app = super::super::tests::fixture(&ctx);
                let recipe = app.project.clone();
                app.generation.results.push(Candidate {
                    identity:None,
                    overview:None,
                    archive: None, archive_issue: String::new(), id: 77, label: "Seed 24680".into(), recipe: recipe.clone(),
                    image: Some(Ok(ctx.load_texture("header-test", egui::ColorImage::filled([8, 8], Color32::GREEN), Default::default()))),
                    zoom: 1., pan: Vec2::ZERO, placed: 0, missing: 0, monuments: vec![],
                    routes: Vec::new(),
                    restrictions: Default::default(), grid: false, no_build: false,
                    map_font: None, background: MAP_BACKGROUND, palette: None,
                });
                let textures = std::cell::RefCell::new(snapshot::Textures::default());
                let render = |app: &mut Studio, events| {
                    let mut output = ctx.run_ui(egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(width, 700.))),
                        events, focused: true, ..Default::default()
                    }, |ui| app.generated_map(ui, 77));
                    textures.borrow_mut().update(&output.textures_delta);
                    output.textures_delta.clear();
                    output
                };
                let label = |output: &egui::FullOutput, name: &str| {
                    output.shapes.iter().find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.job.text == name => {
                            let rect = text.galley.rect.translate(text.pos.to_vec2());
                            assert!(shape.clip_rect.contains_rect(rect), "Clipped {name} at {width}");
                            Some(rect)
                        }
                        _ => None,
                    }).unwrap_or_else(|| panic!("Missing {name} at {width}"))
                };
                let canvas = |output: &egui::FullOutput| {
                    output.shapes.iter().find_map(|shape| match &shape.shape {
                        egui::Shape::Rect(rect) if rect.fill == MAP_BACKGROUND => Some(rect.rect),
                        _ => None,
                    }).unwrap()
                };
                let _ = render(&mut app, vec![]);
                let output = render(&mut app, vec![]);
                let outer = canvas(&output);
                assert!(outer.top() < 40., "Header consumed more than one row at {width}: {outer:?}");
                for name in ["Fit", "View", "Map"] {
                    assert!((label(&output, name).center().y - label(&output, "Fit").center().y).abs() < 2.);
                }
                let click = |app: &mut Studio, point| {
                    let button = |pressed| egui::Event::PointerButton {
                        pos: point, button: egui::PointerButton::Primary, pressed, modifiers: Default::default(),
                    };
                    let _ = render(app, vec![egui::Event::PointerMoved(point), button(true)]);
                    render(app, vec![button(false)])
                };
                let _ = click(&mut app, label(&output, "Map").center());
                let output = render(&mut app, vec![]);
                assert_eq!(canvas(&output), outer);
                for name in ["Export .map…", "Export image…", "Save recipe…"] { label(&output, name); }
                let _ = click(&mut app, label(&output, "Export image…").center());
                assert!(app.export_options_open);
                assert_eq!(app.image_export_target, Some(77));
                assert_eq!(app.project, recipe);
                assert_eq!(app.generation.results[0].recipe, recipe);
                app.generation.edits.get_mut(&77).unwrap().poses.insert(0, [0., 0., 0.]);
                let output = render(&mut app, vec![]);
                click(&mut app, label(&output, "Map").center());
                let output = render(&mut app, vec![]);
                for name in ["Export applied .map…", "Export applied image…", "Save applied recipe…"] {
                    label(&output, name);
                }
                if matches!(width, 280. | 600.)
                    && let Some(directory) = std::env::var_os("MAPPER_MAP_TABS_SNAPSHOTS")
                {
                    let theme = if dark { "dark" } else { "light" };
                    let path = PathBuf::from(directory).join(format!("draft-export-{theme}-{width}.png"));
                    let image = textures.borrow().raster(&ctx, output, [width as usize, 700]);
                    mapper_studio::image_export::write_png(&path, &image, || false).unwrap();
                }
                click(&mut app, outer.center());
                let output = render(&mut app, vec![]);
                click(&mut app, label(&output, "Apply changes").center());
                let output = render(&mut app, vec![]);
                assert_eq!(canvas(&output), outer);
                let header = if width < 300. { "Map issue" } else { "Rebuild blocked" };
                click(&mut app, label(&output, header).center());
                let blocked = (0..12).map(|_| render(&mut app, vec![])).last().unwrap();
                label(&blocked, "Rebuild could not start");
                assert!(blocked.shapes.iter().any(|shape| matches!(&shape.shape,
                    egui::Shape::Text(text) if text.galley.job.text == "The monument to move is unavailable")));
                assert!(app.generation.queue.is_empty());
                assert_eq!(app.generation.edits[&77].poses.get(&0), Some(&[0., 0., 0.]));
                if matches!(width, 280. | 600.)
                    && let Some(directory) = std::env::var_os("MAPPER_MAP_TABS_SNAPSHOTS")
                {
                    let theme = if dark { "dark" } else { "light" };
                    let path = PathBuf::from(directory).join(format!("rebuild-blocked-{theme}-{width}.png"));
                    let image = textures.borrow().raster(&ctx, blocked, [width as usize, 700]);
                    mapper_studio::image_export::write_png(&path, &image, || false).unwrap();
                }
                click(&mut app, outer.center());
                let report = "Road fitting failed\nDetailed constraint.";
                let edit = app.generation.edits.get_mut(&77).unwrap();
                edit.apply_error = None;
                edit.last_issue = Some(EditIssue { id: 78, incomplete: false, detail: report.into() });
                let header = if width < 300. { "Map issue" } else { "Rebuild failed" };
                let output = render(&mut app, vec![]);
                assert_eq!(canvas(&output), outer);
                label(&output, "Apply changes");
                click(&mut app, label(&output, header).center());
                let mut issues = render(&mut app, vec![]);
                for _ in 0..12 {
                    issues = render(&mut app, vec![]);
                }
                label(&issues, "Last rebuild failed");
                label(&issues, "Road fitting failed");
                label(&issues, "Technical report");
                let copied = click(&mut app, label(&issues, "Copy full details").center());
                assert!(copied.platform_output.commands.iter().any(|command| {
                    matches!(command, egui::OutputCommand::CopyText(value) if value == report)
                }));
                if matches!(width, 280. | 600.)
                    && let Some(directory) = std::env::var_os("MAPPER_MAP_TABS_SNAPSHOTS")
                {
                    let theme = if dark { "dark" } else { "light" };
                    let path = PathBuf::from(directory).join(format!("rebuild-issue-{theme}-{width}.png"));
                    let image = textures.borrow().raster(&ctx, issues, [width as usize, 700]);
                    mapper_studio::image_export::write_png(&path, &image, || false).unwrap();
                }
                if width == 360. {
                    click(&mut app, outer.center());
                    app.generation.edits.get_mut(&77).unwrap().last_issue.as_mut().unwrap().incomplete = true;
                    app.generation.results.push(Candidate {
                        identity: None, overview: None, archive: None, archive_issue: report.into(),
                        id: 78, label: "Incomplete map".into(), recipe: recipe.clone(), image: None,
                        zoom: 1., pan: Vec2::ZERO, placed: 0, missing: 0, monuments: vec![],
                        routes: vec![], restrictions: Default::default(), grid: false,
                        no_build: false, map_font: None, background: MAP_BACKGROUND, palette: None,
                    });
                    let output = render(&mut app, vec![]);
                    click(&mut app, label(&output, "Rebuild incomplete").center());
                    let output = render(&mut app, vec![]);
                    label(&output, "Open incomplete map");
                    assert_eq!(canvas(&output), outer);
                    app.generation.results.retain(|map| map.id != 78);
                    let output = render(&mut app, vec![]);
                    assert!(!output.shapes.iter().any(|shape| matches!(&shape.shape,
                        egui::Shape::Text(text) if text.galley.job.text == "Open incomplete map")));

                    click(&mut app, outer.center());
                    app.generation.edits.remove(&77);
                    let detail = format!("Game map could not be saved\n{}END_OF_REPORT", "雪".repeat(5000));
                    app.generation.results[0].archive_issue.clone_from(&detail);
                    let output = render(&mut app, vec![]);
                    click(&mut app, label(&output, "Export blocked").center());
                    let output = (0..12).map(|_| render(&mut app, vec![])).last().unwrap();
                    label(&output, "Game map could not be saved");
                    label(&output, "Export image…");
                    assert_eq!(canvas(&output), outer);
                    if let Some(directory) = std::env::var_os("MAPPER_MAP_TABS_SNAPSHOTS") {
                        let theme = if dark { "dark" } else { "light" };
                        let path = PathBuf::from(directory).join(format!("export-blocked-{theme}-{width}.png"));
                        let image = textures.borrow().raster(&ctx, output.clone(), [width as usize, 700]);
                        mapper_studio::image_export::write_png(&path, &image, || false).unwrap();
                    }
                    click(&mut app, label(&output, "Technical report").center());
                    let output = (0..12).map(|_| render(&mut app, vec![])).last().unwrap();
                    assert!(!output.shapes.iter().any(|shape| matches!(&shape.shape,
                        egui::Shape::Text(text) if text.galley.job.text.contains("END_OF_REPORT"))));
                    let copied = click(&mut app, label(&output, "Copy full details").center());
                    assert!(copied.platform_output.commands.iter().any(|command| {
                        matches!(command, egui::OutputCommand::CopyText(value) if value == &detail)
                    }));
                }
            }
        }
    }

    #[test]
    fn unsupported_native_profile_reports_before_asset_loading() {
        let ctx = egui::Context::default();
        let mut app = super::super::tests::fixture(&ctx);
        app.project.preview_engine = mapper_studio::native_preview::VERSION.into();
        app.project.installation = Some(mapper_studio::installation::Installation {
            kind: mapper_studio::installation::InstallationKind::Client,
            native: mapper_studio::installation::SourceFile {
                relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                sha256: "c".repeat(64),
                bytes: 1,
            },
            scene_manifest: None,
        });
        app.project.native_profile = None;
        let error = app.generation_preflight(&app.project, true).unwrap_err();
        assert!(error.contains("native compatibility profile"), "{error}");
        assert!(error.contains(&"c".repeat(64)), "{error}");
        assert_eq!(
            app.generation_readiness(),
            Err(GenerationBlocker::UnsupportedNativeProfile(error)),
        );
    }

    pub(super) fn require_matching_rust_installation(app: &mut Studio) -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        let library = directory.path().join("RustClient_Data/Plugins/x86_64/RustNative.dll");
        std::fs::create_dir_all(library.parent().unwrap()).unwrap();
        let mut header = [0; 70];
        header[..2].copy_from_slice(b"MZ");
        header[60..64].copy_from_slice(&64_u32.to_le_bytes());
        header[64..].copy_from_slice(b"PE\0\0\x64\x86");
        std::fs::write(library, header).unwrap();
        let identity = mapper_studio::installation::inspect(directory.path(), || false).unwrap();
        let mut profile: mapper_studio::native_profile::Profile = serde_json::from_str(
            include_str!("../../../compatibility/native/client-0f703d43.json"),
        ).unwrap();
        profile.native_sha256.clone_from(&identity.native.sha256);
        app.project.preview_engine = mapper_studio::native_preview::VERSION.into();
        app.project.native_profile = Some(profile);
        app.project.installation = Some(identity);
        directory
    }

    fn select_rust_installation(app: &mut Studio, path: &std::path::Path, ctx: &egui::Context) {
        let recipe = app.project.clone();
        app.source.start(path.into(), &recipe, ctx);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.source.inspecting() {
            app.source.tick(&mut app.project, ctx);
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(app.project, recipe);
    }

    #[test]
    fn generation_readiness_tracks_recipe_edits_and_newly_bound_assets() {
        let ctx = egui::Context::default();
        let mut app = super::super::tests::fixture(&ctx);
        assert!(app.generation_readiness().is_ok());
        let directory = require_matching_rust_installation(&mut app);
        assert_eq!(
            app.generation_readiness(),
            Err(GenerationBlocker::MatchingInstallation),
        );
        select_rust_installation(&mut app, directory.path(), &ctx);
        assert_eq!(
            app.generation_readiness(),
            Err(GenerationBlocker::AssetsUnavailable),
        );
        assert_eq!(app.generation.validated_recipe.as_ref().unwrap().0, app.project);

        app.source.forget(false);
        assert_eq!(
            app.generation_readiness(),
            Err(GenerationBlocker::MatchingInstallation),
        );
        app.project.size = 0;
        let error = app.generation_readiness().unwrap_err();
        assert_eq!(app.generation_preflight(&app.project, true).unwrap_err(), error.message());
        assert!(error.message().contains("World size"), "{error:?}");
    }
    mod snapshot {
        include!("../examples/support/egui_snapshot.rs");
    }
    fn color_fixture(ctx: &egui::Context, composed: bool) -> Studio {
        let mut app=super::super::tests::fixture(ctx);
        app.project.size=1000;app.project.targets.truncate(1);
        app.project.placement_engine=mapper_studio::placement_preview::ROTATION_VERSION.into();
        if !composed {app.project.composition_engine=None;}
        let path=app.project.targets[0].prefab_path.clone().unwrap();
        let palette=app.prefabs.seed_test_cartographic_stamp(ctx,&mut app.project,&path,Some(0));
        app.preview.as_mut().unwrap().key=app.project.terrain_key();
        let recipe=app.project.clone();super::super::tests::deliver_layout(&mut app,recipe,false);
        app.composition.seed_test_view(ctx,&app.project);
        // Read the test layers before disabling the composed view in the native-only case.
        let mut source_project=app.project.clone();source_project.composition_engine=Some(papyrus_terrain::monument_composition::WATER_VERSION.into());
        if !composed {app.composition.seed_test_view(ctx,&source_project);}
        app.composition.seed_test_palette(palette.clone());
        let mut layers=app.composition.view(&source_project).unwrap().maps.layers.clone();
        layers.terrain_seed=app.project.seeds[0].value;layers.biome_seed=app.project.seeds[1].value;
        let cells=layers.surface_resolution.pow(2);
        layers.splat[cells*4..cells*5].fill(255);layers.biome[cells..cells*2].fill(255);
        let preview=app.preview.as_mut().unwrap();preview.palette=Some(palette);
        preview.native_layers=Some(std::sync::Arc::new(layers));
        app.preferences.map_colors.enabled=true;app.preferences.map_colors.overrides[4]=Some([255,0,0]);
        app
    }
    fn queue_ready_recipe(app: &mut Studio, id:u64) {
        app.generation.queue=vec![QueueItem { origin: None,id,label:format!("Seed {}",app.project.seeds[0].value),
            recipe:app.project.clone(),state:State::Running,detail:String::new()}];
        app.generation.run=Some(Run {active_id:id,template:None,start:0,finished:0,terrain_ready:true});
    }

    #[test]
    fn disconnected_generation_workers_fail_their_job_even_with_retained_results() {
        let ctx = egui::Context::default();
        for terrain in [true, false] {
            let mut app = color_fixture(&ctx, false);
            queue_ready_recipe(&mut app, 1);
            let mut next_recipe = app.project.clone();
            next_recipe.seeds[0].value += 1;
            app.generation.queue.push(QueueItem { origin: None,
                id: 2,
                label: "Next seed".into(),
                recipe: next_recipe.clone(),
                state: State::Waiting,
                detail: String::new(),
            });
            assert!(app.preview_current() && app.current_layout().is_some());

            for cancelled in [true, false] {
                let cancel = Arc::new(AtomicBool::new(cancelled));
                if terrain {
                    app.task = Some(preview::Task {
                        ordered_terrain: mapper_studio::native_processing::rivers_follow_terrain(&app.project),
                        key: app.project.terrain_key(),
                        receiver: std::sync::mpsc::channel().1,
                        cancel,
                        progress: Default::default(),
                    });
                    app.poll(&ctx);
                } else {
                    app.placement_task = Some(preview::LayoutTask {
                        receiver: std::sync::mpsc::channel().1,
                        cancel,
                    });
                    app.poll_placement(&ctx);
                }
                if cancelled {
                    assert_eq!(app.generation.queue[0].state, State::Running);
                    assert_eq!(app.generation.queue[1].state, State::Waiting);
                }
            }
            assert_eq!(app.generation.queue[0].state, State::Failed);
            assert_eq!(
                app.generation.queue[0].detail,
                if terrain {
                    "Preview failed: Worker ended unexpectedly."
                } else {
                    "Placement search failed: Worker ended unexpectedly."
                }
            );
            assert!(app.generation.results.is_empty());
            assert_eq!(app.generation.queue[1].state, State::Running);
            assert_eq!(app.generation.active_recipe(), Some(&next_recipe));
            assert_eq!(app.project, next_recipe);
            app.cancel_generation();
        }
    }

    #[test]
    fn generation_at_capacity_keeps_visible_maps_and_pending_edits() {
        let ctx = egui::Context::default();
        let mut app = color_fixture(&ctx, false);
        let new_id = MAX_BATCH as u64 + 2;
        queue_ready_recipe(&mut app, new_id);
        for id in 1..new_id {
            app.generation.results.push(Candidate {
                id, label: format!("Map {id}"), recipe: app.project.clone(),
                image: None, identity: None, overview: None, archive: None,
                archive_issue: String::new(), zoom: 1., pan: Vec2::ZERO, placed: 0,
                missing: 0, monuments: Vec::new(), routes: Vec::new(),
                restrictions: Default::default(), grid: false, no_build: false,
                map_font: None, background: MAP_BACKGROUND, palette: None,
            });
            if !matches!(id, 1 | 2 | 3 | 5) {
                app.generation.edits.entry(id).or_default().poses.insert(0, [5., 5., 0.]);
            }
        }
        for id in (1..=5).rev() {
            app.generation.queue.insert(0, QueueItem { origin: None,
                id, label: format!("Map {id}"), recipe: app.project.clone(),
                state: State::Ready, detail: String::new(),
            });
        }
        let run = app.generation.run.as_mut().unwrap();
        run.start = 5;
        run.finished = 5;
        app.dock = DockState::new(vec![Tab::Generated(1), Tab::Generated(2)]);
        let anchor = app.dock.find_tab(&Tab::Generated(1)).unwrap();
        app.dock.main_surface_mut().split_right(anchor.node, 0.5, vec![Tab::Generated(3)]);
        app.show_tab(Tab::Generated(1));
        app.apply_tab_request();
        assert_eq!(app.visible_generated_tab(), Some(1));
        assert_eq!(app.map_eviction_index().map(|i| app.generation.results[i].id), Some(5));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        while app.generation.busy() && std::time::Instant::now() < deadline {
            let mut output = ctx.run_ui(Default::default(), |ui| app.tick_generation(ui.ctx()));
            output.textures_delta.clear();
            if app.generation.busy() {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
        assert!(!app.generation.busy(), "{}", app.status);
        assert_eq!(app.generation.results.len(), MAX_BATCH);
        for id in [2, 5] {
            assert!(app.generation.results.iter().all(|result| result.id != id));
            assert!(app.generation.queue.iter().all(|item| item.id != id));
            assert!(app.dock.find_tab(&Tab::Generated(id)).is_none());
        }
        for id in [1, 3, new_id] {
            assert!(app.generation.results.iter().any(|result| result.id == id));
            assert!(app.dock.find_tab(&Tab::Generated(id)).is_some());
        }
        assert!(app.generation.edits[&4].has_changes());
        assert_eq!(app.visible_generated_tab(), Some(1));
        assert_eq!(app.map_eviction_index().map(|i| app.generation.results[i].id), Some(new_id));
        app.generation.edits.entry(new_id).or_default().poses.insert(0, [5., 5., 0.]);
        assert_eq!(app.map_eviction_index().map(|i| app.generation.results[i].id), Some(3));
    }

    #[test]
    fn generated_tabs_capture_matching_custom_colors_without_drawing_the_working_map() {
        for composed in [false,true] {
            let ctx=egui::Context::default();let mut app=color_fixture(&ctx,composed);
            let recipe=app.project.clone();
            for (id,color) in [(1,[255,0,0]),(2,[0,0,255])] {
                app.preferences.map_colors.overrides[4]=Some(color);
                let (base,palette)=app.map_color_source().unwrap();
                assert_eq!(app.map_colors.completion_for(&base,&palette,&app.preferences.map_colors),Ok(false));
                queue_ready_recipe(&mut app,id);
                let deadline=std::time::Instant::now()+std::time::Duration::from_secs(90);
                let mut captured=None;
                while app.generation.busy() && std::time::Instant::now()<deadline {
                    let mut output=ctx.run_ui(Default::default(),|ui|app.tick_generation(ui.ctx()));
                    if let Some((_, Some(texture))) = app.exportable_overview()
                        && let Some(deltas) = output.textures_delta.set.get(&texture.id())
                        && let Some(delta) = deltas.iter().find(|delta| delta.pos.is_none())
                    {
                        captured = Some(delta.image.clone());
                    }
                    if let Some(candidate)=app.generation.results.iter().find(|r|r.id==id) {
                        let texture = candidate.image.as_ref().unwrap().as_ref().unwrap();
                        assert_eq!(texture.id(), app.exportable_overview().unwrap().1.unwrap().id());
                        assert!(!output.textures_delta.set.contains_key(&texture.id()));
                    }
                    output.textures_delta.clear();
                    if app.generation.busy() {std::thread::sleep(std::time::Duration::from_millis(2));}
                }
                assert!(!app.generation.busy(),"{}",app.status);
                assert!(app.generation.queue[0].state.retains_result(),"{}",app.status);
                let (base,palette)=app.map_color_source().unwrap();
                let colors=app.map_colors.view_for(&base,&palette,&app.preferences.map_colors).unwrap();
                let Some(egui::ImageData::Color(captured))=captured else {panic!("No accepted texture upload")};
                assert_eq!(captured.size,[2048,2048]);
                assert!(std::sync::Arc::ptr_eq(&captured, &colors.image));
                if !composed {
                    let center=captured.pixels[1024*2048+1024];
                    assert!(if id==1 {center.r()>center.b()} else {center.b()>center.r()},"{center:?}");
                }
                assert_eq!(app.exportable_map(),Some(colors.image.as_ref()));
                assert_eq!(app.project,recipe);
            }
        }
    }
    #[test]
    fn color_render_failure_fails_the_queue_instead_of_capturing_default_colors() {
        let ctx=egui::Context::default();let mut app=color_fixture(&ctx,false);
        let mut malformed=(**app.preview.as_ref().unwrap().native_layers.as_ref().unwrap()).clone();
        malformed.height.pop();app.preview.as_mut().unwrap().native_layers=Some(std::sync::Arc::new(malformed));
        queue_ready_recipe(&mut app,1);
        let deadline=std::time::Instant::now()+std::time::Duration::from_secs(90);
        while app.generation.busy() && std::time::Instant::now()<deadline {
            app.tick_generation(&ctx);std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(!app.generation.busy());
        assert_eq!(app.generation.queue[0].state,State::Failed,"{}",app.status);
        assert!(app.generation.results.is_empty());
        assert!(app.status.starts_with("Map colors:"),"{}",app.status);
    }
    #[test]
    fn late_prepared_defaults_wait_until_the_generation_queue_is_idle() {
        let ctx = egui::Context::default();
        let mut app = super::super::tests::fixture(&ctx);
        let installation = require_matching_rust_installation(&mut app);
        select_rust_installation(&mut app, installation.path(), &ctx);
        app.project.composition_engine =
            Some(papyrus_terrain::monument_composition::WATER_VERSION.into());
        let rivers = mapper_studio::river_generation::Recipe {
            engine: papyrus_terrain::river_pipeline::VERSION.into(),
            source_sha256: "b".repeat(64),
            source_document: Some("embedded fixture".into()),
            objects: None,
        };
        app.project.rivers = Some(rivers.clone());
        let recipe = app.project.clone();
        let objects = mapper_studio::river_objects::Recipe {
            source_sha256: "c".repeat(64),
            infrastructure_sha256: "d".repeat(64),
            catalog_sha256: "e".repeat(64),
            documents: None,
        };
        app.water.accept_prepared(
            installation.path().to_owned(),
            recipe.installation.clone().unwrap(),
            Some(mapper_studio::river_generation::Recipe {
                objects: Some(objects.clone()),
                ..rivers
            }),
        );
        app.generation.queue.push(QueueItem { origin: None,
            id: 1,
            label: "Frozen recipe".into(),
            recipe: recipe.clone(),
            state: State::Running,
            detail: String::new(),
        });
        app.generation.run = Some(Run {
            active_id: 1,
            template: None,
            start: 0,
            finished: 0,
            terrain_ready: false,
        });

        app.tick_preferences(&ctx);
        app.apply_prepared_recipe_defaults();
        assert!(app.generation.busy());
        assert_eq!(app.generation.active_recipe(), Some(&recipe));
        assert_eq!(app.project, recipe);
        assert!(!app.history.can_undo());

        app.cancel_generation();
        app.tick_preferences(&ctx);
        app.apply_prepared_recipe_defaults();
        assert_eq!(app.project.rivers.as_ref().unwrap().objects.as_ref(), Some(&objects));
        assert_eq!(
            app.project.composition_engine.as_deref(),
            Some(papyrus_terrain::monument_composition::SOURCE_HEIGHT_VERSION),
        );
        assert_eq!(app.generation.queue[0].recipe, recipe);
    }

    #[test]
    fn rejected_generation_preserves_the_working_recipe_history_and_queue() {
        let ctx = egui::Context::default();
        let mut app = super::super::tests::fixture(&ctx);
        drop(require_matching_rust_installation(&mut app));
        let recipe = app.project.clone();
        let mut normalized = recipe.clone();
        mapper_studio::generation_defaults::apply(&mut normalized);
        assert_ne!(normalized, recipe);
        app.saved = recipe.clone();

        let mut older = recipe.clone();
        older.master_seed += 1;
        older.refresh_inherited_seeds();
        app.history.record(older.clone(), &recipe);
        app.generation.queue.push(QueueItem { origin: None,
            id: 1,
            label: "Retained failed map".into(),
            recipe: older.clone(),
            state: State::Failed,
            detail: "Retained generation issue".into(),
        });
        let queue = serde_json::to_value(&app.generation.queue).unwrap();
        let revision = app.generation.revision;
        let next_id = app.generation.next_id;

        app.begin_generation(BatchPlan::exact(recipe.clone()), &ctx);
        assert_eq!(app.status, GenerationBlocker::MatchingInstallation.message());
        assert_eq!(app.project, recipe);
        assert!(!app.dirty());
        assert!(!app.generation.busy());
        assert!(app.task.is_none());
        assert_eq!(app.generation.revision, revision);
        assert_eq!(app.generation.next_id, next_id);
        assert_eq!(serde_json::to_value(&app.generation.queue).unwrap(), queue);
        app.history.undo(&mut app.project);
        assert_eq!(app.project, older);
        app.history.redo(&mut app.project);
        assert_eq!(app.project, recipe);
    }

    #[test]
    fn applying_map_changes_keeps_the_saved_native_generation_version() {
        let ctx = egui::Context::default();
        let mut app = super::super::tests::fixture(&ctx);
        let installation = app.prefabs.seed_test_verified_stamp(&ctx, &mut app.project);
        let mut profile: mapper_studio::native_profile::Profile = serde_json::from_str(
            include_str!("../../../compatibility/native/client-0f703d43.json"),
        ).unwrap();
        profile.native_sha256.clone_from(&app.project.installation.as_ref().unwrap().native.sha256);
        app.project.preview_engine = mapper_studio::native_preview::VERSION.into();
        app.project.native_profile = Some(profile);
        app.project.composition_engine = Some(papyrus_terrain::monument_composition::WATER_VERSION.into());
        select_rust_installation(&mut app, installation.path(), &ctx);
        let saved = app.project.clone();
        queue_ready_recipe(&mut app, 1);
        app.generation.next_id = 2;

        // Queue behind an active map so the synthetic installation is never
        // used by a terrain worker.
        let mut edited = saved.clone();
        edited.targets[0].x = 125.;
        let job = app.enqueue_generation(BatchPlan::exact(edited.clone()), &ctx, true).unwrap();
        assert_eq!(app.generation.queue[1].id, job);
        assert_eq!(app.generation.queue[1].recipe, edited);

        app.begin_generation(BatchPlan::exact(saved.clone()), &ctx);
        let mut expected = saved.clone();
        mapper_studio::generation_defaults::apply(&mut expected);
        assert_ne!(expected.composition_engine, saved.composition_engine);
        assert_eq!(app.generation.queue[2].recipe, expected);
        assert_eq!(app.generation.active_recipe(), Some(&saved));
    }

    #[test]
    fn hidden_queued_terrain_variant_uses_a_thumbnail_only_until_composition() {
        let ctx = egui::Context::default();
        let mut app = super::super::tests::fixture(&ctx);
        app.live_generation = false;
        assert!(app.preview_presentation().detailed);
        let draft = app.project.clone();
        app.project.size += 100;
        app.project.preview_engine = mapper_studio::native_preview::VERSION.into();
        app.project.composition_engine = Some(
            papyrus_terrain::monument_composition::SOURCE_HEIGHT_VERSION.into(),
        );
        app.generation.queue.push(QueueItem { origin: None,
            id: 1,
            label: "Queued variant".into(),
            recipe: app.project.clone(),
            state: State::Running,
            detail: String::new(),
        });
        app.generation.run = Some(Run {
            start: 0,
            active_id: 1,
            finished: 0,
            template: Some(draft),
            terrain_ready: false,
        });
        assert!(app.preview_presentation().detailed);
        app.show_tab(Tab::Generated(2));
        app.apply_tab_request();
        assert!(!app.preview_presentation().detailed);
        app.live_generation = true;
        let presentation = app.preview_presentation();
        assert!(presentation.live);
        assert!(!presentation.detailed);
        app.generation.run.as_mut().unwrap().template = Some(app.project.clone());
        assert!(app.preview_presentation().detailed);
    }

    #[test]
    fn generate_replaces_background_preview_and_queues_another_recipe() {
        let ctx = egui::Context::default();
        let mut app = super::super::tests::fixture(&ctx);
        app.request_preview(&ctx, false);
        let preview_cancel = app.task.as_ref().unwrap().cancel.clone();
        let render = |app: &mut Studio, events| {
            let mut output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(900., 600.))),
                focused: true,
                events,
                ..Default::default()
            }, |ui| app.recipe_generate(ui));
            output.textures_delta.clear();
            output
        };
        let output = render(&mut app, vec![]);
        let pos = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(label) if label.galley.job.text == "Queue" => {
                Some(label.pos + label.galley.rect.center().to_vec2())
            }
            _ => None,
        }).unwrap();
        let click_generate = |app: &mut Studio| {
            for pressed in [true, false] {
                let _ = render(app, vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton {
                    pos, pressed, button: egui::PointerButton::Primary, modifiers: Default::default(),
                }]);
            }
        };
        click_generate(&mut app);
        assert!(preview_cancel.load(Ordering::Relaxed));
        assert_eq!(app.generation.queue[0].state, State::Running);
        let running = app.project.clone();
        app.with_recipe_draft(|studio| studio.project.size += 100);
        assert!(render(&mut app, vec![]).shapes.iter().any(|shape| {
            matches!(&shape.shape, egui::Shape::Text(label) if label.galley.job.text == "Queue")
        }));
        click_generate(&mut app);
        assert_eq!(app.project, running);
        assert_eq!(app.generation.queue[1].state, State::Waiting);
        assert_eq!(app.generation.queue[1].recipe.size, running.size + 100);
        app.cancel_generation();
    }

    #[test]
    fn monument_controls_edit_the_next_map_without_interrupting_generation() {
        let ctx = egui::Context::default();
        workbench_style::install(&ctx);
        let mut app = super::super::tests::fixture(&ctx);
        app.project.size = 1000;
        app.begin_generation(BatchPlan::exact(app.project.clone()), &ctx);
        let active = app.project.clone();
        let active_selection = app.selected;
        let target = &active.targets[1];
        let target_id = target.instance_id;
        let display_name = app.monument_icons.name(target.prefab_path.as_deref(), &target.name);
        let textures = std::cell::RefCell::new(snapshot::Textures::default());
        let size = [235, 540];
        let render = |app: &mut Studio, mut tab, events| {
            let mut output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(size[0] as f32, size[1] as f32))),
                events,
                focused: true,
                ..Default::default()
            }, |ui| Tabs::new(app, None, &DockState::new(vec![tab])).ui(ui, &mut tab));
            textures.borrow_mut().update(&output.textures_delta);
            output.textures_delta.clear();
            output
        };
        let label = |output: &egui::FullOutput, name: &str| {
            output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == name => {
                    let point = text.pos + text.galley.rect.center().to_vec2();
                    shape.clip_rect.contains(point).then_some(point)
                }
                _ => None,
            })
        };
        let click = |app: &mut Studio, tab, pos| {
            for pressed in [true, false] {
                let _ = render(app, tab, vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton {
                    pos, pressed, button: egui::PointerButton::Primary, modifiers: Default::default(),
                }]);
            }
        };
        let _ = render(&mut app, Tab::Outliner, vec![]);
        let baseline = app.generation.draft_snapshot.as_ref().unwrap().targets.as_ptr();
        let outliner = render(&mut app, Tab::Outliner, vec![]);
        let point = label(&outliner, &display_name).expect("The recipe outliner shows the target");
        click(&mut app, Tab::Outliner, point);
        assert_eq!(app.generation.draft_target, Some(target_id));
        assert_eq!(app.selected, active_selection);
        let _ = render(&mut app, Tab::Inspector, vec![]);
        let inspector = render(&mut app, Tab::Inspector, vec![]);
        assert_eq!(app.generation.draft_snapshot.as_ref().unwrap().targets.as_ptr(), baseline);
        assert!(label(&inspector, "Next map").is_some());
        assert!(label(&inspector, "Regenerate monuments").is_none());
        let remove = label(&inspector, "Remove monument").expect("The draft target can be removed");
        if let Some(directory) = std::env::var_os("MAPPER_MAP_TABS_SNAPSHOTS") {
            let directory = PathBuf::from(directory);
            for (name, output) in [("draft-monuments.png", outliner), ("draft-selection.png", inspector)] {
                let image = textures.borrow().raster(&ctx, output, size);
                mapper_studio::image_export::write_png(&directory.join(name), &image, || false).unwrap();
            }
        }
        click(&mut app, Tab::Inspector, remove);
        assert!(app.generation.draft_snapshot.is_none());
        assert_eq!(app.project, active);
        assert!(app.document_recipe().targets.iter().all(|target| target.instance_id != target_id));
        assert!(app.undo_generation_draft(false));
        assert!(app.document_recipe().targets.iter().any(|target| target.instance_id == target_id));
        assert!(app.undo_generation_draft(true));

        app.generate_recipe(&ctx);
        assert_eq!(app.generation.queue[0].state, State::Running);
        assert_eq!(app.generation.queue[0].recipe, active);
        assert_eq!(app.generation.queue[1].state, State::Waiting);
        assert!(app.generation.queue[1].recipe.targets.iter().all(|target| target.instance_id != target_id));
        app.cancel_generation();
        assert_eq!(app.project.targets.len(), active.targets.len() - 1);
        assert!(app.selected.is_none());
        assert!(app.generation.draft_snapshot.is_none());
    }

    #[test]
    fn closing_the_last_map_restores_the_recipe_preview_in_its_dock() {
        let ctx = egui::Context::default();
        let mut app = super::super::tests::fixture(&ctx);
        let anchor = app.dock.find_tab(&Tab::Home).unwrap();
        for tab in [Tab::Generated(1), Tab::Generated(2)] {
            app.show_tab(tab);
            app.apply_tab_request();
        }
        assert!(app.dock.find_tab(&Tab::Home).is_some());
        let mut dock = std::mem::replace(&mut app.dock, DockState::new(vec![]));
        let mut tabs = Tabs::new(&mut app, Some(2), &dock);
        tabs.on_close(&mut Tab::Generated(1));
        tabs.on_close(&mut Tab::Generated(1));
        tabs.on_close(&mut Tab::Generated(2));
        tabs.finish(&mut dock);
        assert_eq!(dock.find_tab(&Tab::Home), Some(anchor));
    }

    #[test]
    fn named_recipe_generation_keeps_queued_snapshots_and_active_seed_independent() {
        let ctx = egui::Context::default();
        let mut app = crate::tests::fixture(&ctx);
        let first = app.project.clone();
        let mut island = first.clone();
        island.master_seed = 45678;
        island.refresh_inherited_seeds();
        assert!(app.import_named_recipe("Island Test", island.clone()));
        let second = app.recipe_tabs.active();
        app.activate_recipe(FIRST_RECIPE);
        app.generation.queue_paused = true;

        app.generate_recipe_for(second, Submission::Queue, &ctx);
        assert_eq!(app.recipe_tabs.active(), FIRST_RECIPE);
        let queued = &app.generation.queue[0];
        assert_eq!(queued.origin.as_ref().unwrap().name, "Island Test");
        assert_eq!(queued.recipe.master_seed, island.master_seed);
        let working = app.project.clone();
        assert_eq!(app.document_recipe(), &first);

        app.apply_queue_action(Some(QueueAction::UseSeed(789)), &ctx);
        assert_eq!(app.document_recipe().master_seed, 789);
        assert_eq!(app.project, working);
        app.activate_recipe(second);
        assert_eq!(app.document_recipe(), &island);
        assert_eq!(app.project, working);

        app.with_recipe_editor(FIRST_RECIPE, &ctx, |studio| {
            studio.project.master_seed = 987;
            studio.project.refresh_inherited_seeds();
        });
        assert_eq!(app.recipe_by_id(FIRST_RECIPE).master_seed, 987);
        assert_eq!(app.document_recipe(), &island);
        assert_eq!(app.generation.queue[0].recipe.master_seed, island.master_seed);
        app.finish_generation_run();
        assert_eq!(app.document_recipe(), &island);
        app.activate_recipe(FIRST_RECIPE);
        assert_eq!(app.document_recipe().master_seed, 987);
        app.apply_queue_action(Some(QueueAction::UseSeed(555)), &ctx);
        assert_eq!(app.document_recipe().master_seed, 555);
        app.history.undo(&mut app.project);
        assert_eq!(app.document_recipe().master_seed, 987);
    }

    #[test]
    fn named_recipe_docked_panes_render_two_independent_generate_buttons() {
        let ctx = egui::Context::default();
        workbench_style::install(&ctx);
        let mut app = crate::tests::fixture(&ctx);
        let mut other = app.project.clone();
        other.master_seed = 45678;
        other.refresh_inherited_seeds();
        assert!(app.import_named_recipe("Island Test", other));
        let second = app.recipe_tabs.active();
        app.activate_recipe(FIRST_RECIPE);

        let mut dock = DockState::new(vec![Tab::Recipe(FIRST_RECIPE)]);
        dock.main_surface_mut().split_right(NodeIndex::root(), 0.5, vec![Tab::Recipe(second)]);
        let mut textures = snapshot::Textures::default();
        for _ in 0..2 {
            let mut frame = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(940., 620.))),
                    ..Default::default()
                },
                |ui| {
                    let mut tabs = Tabs::new(&mut app, None, &dock);
                    DockArea::new(&mut dock).style(workbench_style::dock(ui)).show_inside(ui, &mut tabs);
                    tabs.finish(&mut dock);
                },
            );
            textures.update(&frame.textures_delta);
            frame.textures_delta.clear();
            let labels = frame.shapes.iter().filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == "Generate" => Some(text.pos.x),
                _ => None,
            }).collect::<Vec<_>>();
            if let [first, second] = labels.as_slice() {
                assert!((first - second).abs() > 300.);
                assert_eq!(app.recipe_tabs.active(), FIRST_RECIPE);
                return;
            }
        }
        panic!("Each docked recipe should render a Generate button");
    }

    #[test]
    fn enqueuing_uses_the_current_draft_without_interrupting_the_active_recipe() {
        let ctx = egui::Context::default();
        let mut app = super::super::tests::fixture(&ctx);
        let first = app.project.clone();
        app.generate_recipe(&ctx);
        let active_id = app.generation.run.as_ref().unwrap().active_id;
        let cancel = app.task.as_ref().unwrap().cancel.clone();
        app.with_recipe_draft(|studio| {
            studio.project.master_seed += 1;
            studio.project.refresh_inherited_seeds();
        });
        let second = app.document_recipe().clone();
        assert_ne!(first.master_seed, second.master_seed);
        assert_eq!(app.project, first);
        assert!(app.generation_readiness().is_ok());
        app.generate_recipe(&ctx);
        assert_eq!(app.generation.run.as_ref().unwrap().active_id, active_id);
        assert!(!cancel.load(Ordering::Relaxed));
        assert_eq!(app.generation.queue[0].state, State::Running);
        assert_eq!(app.generation.queue[0].recipe, first);
        assert_eq!(app.generation.queue[1].state, State::Waiting);
        assert_eq!(app.generation.queue[1].recipe, second);
        app.cancel_generation();
        assert_eq!(app.project, second);
    }

    #[test]
    fn saved_roadside_markers_edit_the_original_request_without_adding_land_targets() {
        let mut recipe = Project::default();
        let mut roads = mapper_studio::infrastructure::Recipe::new("a".repeat(64));
        roads.staged = Some(mapper_studio::road_generation::Policy::for_new_recipe(
            "b".repeat(64), None, false,
        ).unwrap());
        recipe.roads = Some(roads);
        let path = "assets/bundled/prefabs/autospawn/monument/roadside/gas_station_1.prefab";
        // A marker saved before roadside editing existed carries this occurrence ID.
        let monuments = [map_monuments::Monument {
            id: 10661240344236727918,
            name: "Gas Station".into(),
            prefab: Some(path.into()),
            position: [707., 49., -212.],
            yaw: 332.,
            restriction_origin: None,
        }];
        assert!(movable_monument(&recipe, &monuments[0]));
        let mut different = monuments[0].clone();
        different.prefab = Some("assets/bundled/prefabs/autospawn/monument/offshore/oilrig_1.prefab".into());
        assert!(!movable_monument(&recipe, &different));

        let mut draft = MapEdit::default();
        draft.poses.insert(0, [550., 988., 15.]);
        let moved = draft.recipe(&recipe, &monuments).unwrap();
        assert_eq!(moved.targets, recipe.targets);
        assert_eq!(moved.placement_engine, recipe.placement_engine);
        assert_eq!(moved.terrain_key(), recipe.terrain_key());
        assert_eq!(moved.seeds, recipe.seeds);
        assert_eq!(moved.roads.as_ref().unwrap().staged.as_ref().unwrap()
            .roadside_preferences[0].max_displacement_m, Some(150));
        draft.move_limit_m = Some(75);
        draft.poses.insert(0, [560., 990., 30.]);
        let second = draft.recipe(&moved, &monuments).unwrap();
        let saved = &second.roads.as_ref().unwrap().staged.as_ref().unwrap().roadside_preferences;
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].request, monuments[0].id);
        assert_eq!(saved[0].position, [560., 990.]);
        assert_eq!(saved[0].yaw, 30.);
        assert_eq!(saved[0].max_displacement_m, Some(75));

        let ctx = egui::Context::default();
        let mut app = super::super::tests::fixture(&ctx);
        app.generation.results.push(Candidate {
            identity: None, overview: None, archive: None, archive_issue: String::new(),
            id: 77, label: "Roadside edit".into(), recipe: second, image: None,
            zoom: 1., pan: Vec2::ZERO, placed: 0, missing: 0, monuments: Vec::new(),
            routes: Vec::new(), restrictions: Default::default(), grid: false, no_build: false,
            map_font: None, background: MAP_BACKGROUND, palette: None,
        });
        let warns = |app: &mut Studio| {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| app.generated_summary(ui, 77));
            output.textures_delta.clear();
            output.shapes.iter().any(|shape| matches!(&shape.shape,
                    egui::Shape::Text(text) if text.galley.job.text == "1 roadside monument could not be placed"))
        };
        assert!(warns(&mut app));
        app.generation.results[0].recipe.roads.as_mut().unwrap().staged.as_mut().unwrap()
            .roadside_preferences[0].presence = Some(Presence::Excluded);
        assert!(!warns(&mut app));
    }

    #[test]
    fn clicking_and_dragging_routes_stages_spline_controls_without_panning() {
        use mapper_studio::path_edit::{Key, Kind, Operation, Route};
        let ctx = egui::Context::default();
        let mut app = super::super::tests::fixture(&ctx);
        let mut recipe = app.project.clone();
        recipe.size = 3500;
        let prefab = "assets/test.prefab";
        app.prefabs.seed_test_stamp(&ctx, &mut recipe, prefab);
        let target = &mut recipe.targets[0];
        target.prefab_path = Some(prefab.into());
        target.presence = Presence::Allowed;
        let monument = map_monuments::Monument {
            id: target.instance_id,
            name: target.name.clone(),
            prefab: target.prefab_path.clone(),
            position: [-500., 15., 0.],
            yaw: 0.,
            restriction_origin: None,
        };
        let id = 18;
        let texture = ctx.load_texture(
            "spline-test", egui::ColorImage::filled([8, 8], Color32::GREEN), Default::default(),
        );
        let texture_id = texture.id();
        let mut routes: Vec<_> = [Kind::Road, Kind::Trail, Kind::Rail].into_iter().enumerate().map(|(index, kind)| {
            let z = (index as f32 - 1.) * 550.;
            let mut controls = vec![[-1000., z], [0., z], [1000., z]];
            if kind == Kind::Trail {
                controls.insert(1, [-500., z]);
            }
            Route {
                key: Key { kind, index },
                fixed: vec![0, controls.len() - 1],
                controls,
            }
        }).collect();
        let dense = Key { kind: Kind::Road, index: 3 };
        routes.push(Route {
            key: dense,
            fixed: vec![0, 100],
            controls: (0..=100).map(|i| [i as f32 * 10. - 500., -270.]).collect(),
        });
        app.generation.results.push(Candidate {
            identity: None, overview: None, archive: None, archive_issue: String::new(),
            id, label: "Spline test".into(), recipe: recipe.clone(), image: Some(Ok(texture)),
            zoom: 2., pan: Vec2::ZERO, placed: 1, missing: 0, monuments: vec![monument], routes,
            restrictions: Default::default(), grid: false, no_build: false, map_font: None,
            background: MAP_BACKGROUND, palette: None,
        });
        app.show_tab(Tab::Generated(id));
        app.apply_tab_request();
        let textures = std::cell::RefCell::new(snapshot::Textures::default());
        let time = std::cell::Cell::new(0.);
        let render = |app: &mut Studio, events| {
            time.set(time.get() + 1. / 60.);
            let mut output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(880., 680.))),
                time: Some(time.get()),
                events, focused: true, ..Default::default()
            }, |ui| {
                app.toolbar(ui);
                app.generated_map(ui, id);
            });
            textures.borrow_mut().update(&output.textures_delta);
            output.textures_delta.clear();
            output
        };
        let button = |pos, pressed| egui::Event::PointerButton {
            pos, pressed, button: egui::PointerButton::Primary, modifiers: Default::default(),
        };
        let _ = render(&mut app, vec![]);
        let output = render(&mut app, vec![]);
        let map = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Mesh(mesh) if mesh.texture_id == texture_id => Some(mesh.calc_bounds()),
            _ => None,
        }).unwrap();
        let shared = edit_screen(map, recipe.size, -420., 0.);
        assert!(shared.distance(edit_screen(map, recipe.size, -500., 0.)) > MONUMENT_PICK_RADIUS);
        assert!(app.prefabs.stamp(&recipe, prefab).unwrap()
            .hit_test_source([-420., 0.], [-500., 0., 0.], false, false));
        for z in [-550., 0., 550.] {
            // The trail passes through the monument footprint at this pick.
            let segment = edit_screen(map, recipe.size, -420., z);
            let _ = render(&mut app, vec![egui::Event::PointerMoved(segment), button(segment, true)]);
            let _ = render(&mut app, vec![button(segment, false)]);
            let control = edit_screen(map, recipe.size, 0., z);
            let before = app.generation.edits[&id].paths.changes().len();
            let jitter = control + egui::vec2(0.75, -0.5);
            let _ = render(&mut app, vec![egui::Event::PointerMoved(control), button(control, true)]);
            let _ = render(&mut app, vec![egui::Event::PointerMoved(jitter), button(jitter, false)]);
            assert_eq!(app.generation.edits[&id].paths.changes().len(), before);
            let end = control + egui::vec2(6., -15.);
            let _ = render(&mut app, vec![egui::Event::PointerMoved(control), button(control, true)]);
            let _ = render(&mut app, vec![egui::Event::PointerMoved(end)]);
            let _ = render(&mut app, vec![button(end, false)]);
        }
        let changes = app.generation.edits[&id].paths.changes();
        assert_eq!(changes.len(), 3);
        assert_eq!(changes.iter().map(|c| c.route.kind).collect::<Vec<_>>(), [Kind::Road, Kind::Trail, Kind::Rail]);
        assert_eq!(changes.iter().map(|c| c.control).collect::<Vec<_>>(), [1, 2, 1]);
        assert!(changes.iter().all(|c| matches!(c.operation, Operation::Move(_))));
        let staged = changes.to_vec();
        assert_eq!(app.focused_map_canvas(&ctx), Some(id));
        let _ = render(&mut app, vec![history_key(egui::Key::Z, false)]);
        assert_eq!(app.generation.edits[&id].paths.changes(), &staged[..2]);
        let _ = render(&mut app, vec![history_key(egui::Key::Z, true)]);
        assert_eq!(app.generation.edits[&id].paths.changes(), staged);
        assert_eq!(app.generation.results[0].pan, Vec2::ZERO);
        assert!(!app.generation.busy());
        assert!(app.generation.results[0].recipe.path_edits.is_empty());
        let anchor = edit_screen(map, recipe.size, -1000., 550.);
        let dragged_anchor = anchor + egui::vec2(30., 20.);
        let _ = render(&mut app, vec![egui::Event::PointerMoved(anchor), button(anchor, true)]);
        let _ = render(&mut app, vec![egui::Event::PointerMoved(dragged_anchor)]);
        let _ = render(&mut app, vec![button(dragged_anchor, false)]);
        let delete = || egui::Event::Key {
            key: egui::Key::Delete, physical_key: None, pressed: true,
            repeat: false, modifiers: Default::default(),
        };
        let _ = render(&mut app, vec![delete()]);
        assert_eq!(app.generation.edits[&id].paths.changes(), staged);
        assert_eq!(app.generation.results[0].pan, Vec2::ZERO);
        let corner = map.center() + egui::vec2(130., 180.);
        let end = corner + egui::vec2(24., 13.);
        let _ = render(&mut app, vec![egui::Event::PointerMoved(corner), button(corner, true)]);
        let _ = render(&mut app, vec![egui::Event::PointerMoved(end)]);
        let _ = render(&mut app, vec![button(end, false)]);
        assert_eq!(app.generation.results[0].pan, end - corner);
        assert_eq!(app.generation.edits[&id].paths.changes().len(), 3);
        let segment = edit_screen(map.translate(end - corner), recipe.size, 590., 550.)
            + egui::vec2(0., -7.);
        // Start a new click sequence after selecting and dragging the rail.
        time.set(time.get() + 1.);
        for _ in 0..2 {
            let _ = render(&mut app, vec![egui::Event::PointerMoved(segment), button(segment, true)]);
            let _ = render(&mut app, vec![button(segment, false)]);
        }
        let inserted = *app.generation.edits[&id].paths.changes().last().unwrap();
        assert_eq!(inserted.route.kind, Kind::Rail);
        assert_eq!(inserted.control, 1);
        assert!(matches!(inserted.operation, Operation::Insert(_)));
        let _ = render(&mut app, vec![delete()]);
        let removed = app.generation.edits[&id].paths.changes().last().unwrap();
        assert_eq!(removed.route.kind, Kind::Rail);
        assert_eq!(removed.control, 2);
        assert_eq!(removed.operation, Operation::Remove);
        let _ = render(&mut app, vec![history_key(egui::Key::Z, false)]);
        let _ = render(&mut app, vec![history_key(egui::Key::Z, false)]);
        let right_button = |pos, pressed| egui::Event::PointerButton {
            pos, pressed, button: egui::PointerButton::Secondary, modifiers: Default::default(),
        };
        let _ = render(&mut app, vec![egui::Event::PointerMoved(segment), right_button(segment, true)]);
        let _ = render(&mut app, vec![right_button(segment, false)]);
        let menu = render(&mut app, vec![]);
        let add = menu.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(label) if label.galley.job.text == "Add point" => {
                Some(label.pos + label.galley.size() * 0.5)
            }
            _ => None,
        }).expect("Right-clicking a rail opens its point actions");
        let _ = render(&mut app, vec![egui::Event::PointerMoved(add), button(add, true)]);
        let _ = render(&mut app, vec![button(add, false)]);
        let changes = app.generation.edits[&id].paths.changes();
        assert_eq!(changes.len(), 4);
        assert_eq!(changes.last(), Some(&inserted));
        let footprint = shared + (end - corner);
        time.set(time.get() + 1.);
        let route_hover = render(&mut app, vec![egui::Event::PointerMoved(footprint)]);
        assert_eq!(route_hover.platform_output.cursor_icon, egui::CursorIcon::PointingHand);
        let _ = render(&mut app, vec![egui::Event::PointerMoved(footprint), button(footprint, true)]);
        let _ = render(&mut app, vec![button(footprint, false)]);
        let center = edit_screen(map.translate(end - corner), recipe.size, -500., 0.);
        let marker_hover = render(&mut app, vec![egui::Event::PointerMoved(center)]);
        assert_eq!(marker_hover.platform_output.cursor_icon, egui::CursorIcon::Grab);
        let center_destination = center + egui::vec2(20., 23.);
        let _ = render(&mut app, vec![egui::Event::PointerMoved(center), button(center, true)]);
        let _ = render(&mut app, vec![egui::Event::PointerMoved(center_destination), button(center_destination, false)]);
        assert_eq!(app.generation.edits[&id].poses.len(), 1, "The monument center owns the drag");
        assert_eq!(app.generation.edits[&id].paths.changes().len(), 4);
        let _ = render(&mut app, vec![history_key(egui::Key::Z, false)]);
        assert!(app.generation.edits[&id].poses.is_empty());
        let destination = footprint + egui::vec2(-21., 24.);
        let _ = render(&mut app, vec![egui::Event::PointerMoved(footprint), button(footprint, true)]);
        let _ = render(&mut app, vec![egui::Event::PointerMoved(destination), button(destination, false)]);
        assert_eq!(app.generation.edits[&id].poses.len(), 1);
        assert_eq!(app.generation.edits[&id].paths.changes().len(), 4);
        assert_eq!(app.generation.results[0].pan, end - corner);
        assert!(!app.generation.busy());
        let last = render(&mut app, vec![]);
        assert!(last.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Text(label) if label.galley.job.text == "Apply changes")));
        assert!(last.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Text(label) if label.galley.job.text == "Seeds")));
        if let Some(directory) = std::env::var_os("MAPPER_MAP_TABS_SNAPSHOTS") {
            let path = PathBuf::from(directory).join("direct-spline-draft.png");
            let image = textures.borrow().raster(&ctx, last, [880, 680]);
            mapper_studio::image_export::write_png(&path, &image, || false).unwrap();
        }
        let result = &app.generation.results[0];
        let edit = &app.generation.edits[&id];
        let expected = edit.recipe(&result.recipe, &result.monuments).unwrap();
        assert_eq!(expected.path_edits, edit.paths.changes());
        assert_ne!(expected.targets, result.recipe.targets);
        assert_eq!(expected.seeds, result.recipe.seeds);

        let dense_pointer = edit_screen(map.translate(end - corner), recipe.size, 245., -270.);
        let pulled = dense_pointer + egui::vec2(3., -21.);
        let _ = render(&mut app, vec![egui::Event::PointerMoved(dense_pointer), button(dense_pointer, true)]);
        let _ = render(&mut app, vec![button(dense_pointer, false)]);
        time.set(time.get() + 1.);
        let _ = render(&mut app, vec![button(dense_pointer, true)]);
        let _ = render(&mut app, vec![egui::Event::PointerMoved(pulled), button(pulled, false)]);
        let changes = app.generation.edits[&id].paths.changes();
        assert_eq!(changes.len(), 5);
        assert_eq!(changes[4].route, dense);
        assert!(matches!(changes[4].operation, Operation::Bend { .. }));
        assert_eq!(app.generation.results[0].pan, end - corner);
    }

    fn history_key(key: egui::Key, shift: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                command: true,
                ctrl: true,
                shift,
                ..Default::default()
            },
        }
    }

    #[test]
    fn monument_drags_stage_together_and_apply_in_the_existing_map_tab() {
        let ctx = egui::Context::default();
        let mut app = super::super::tests::fixture(&ctx);
        let working = app.project.clone();
        let mut recipe = working.clone();
        recipe.targets.iter_mut().for_each(|target| {
            target.presence = Presence::Allowed;
            target.radius = 550.;
        });
        let id = 77;
        let image = ctx.load_texture("edit-test", egui::ColorImage::filled([8, 8], Color32::GREEN), Default::default());
        let texture_id = image.id();
        let monuments = recipe.targets.iter().map(|target| map_monuments::Monument {
            id: target.instance_id, name: target.name.clone(), prefab: target.prefab_path.clone(),
            position: [target.x, 15., target.z], yaw: 30., restriction_origin: None,
        }).collect();
        app.generation.results.push(Candidate {
            identity: None, overview: None, archive: None, archive_issue: String::new(),
            id, label: "Edited map".into(), recipe: recipe.clone(),
            image: Some(Ok(image)), zoom: 1., pan: Vec2::ZERO,
            placed: 3, missing: 0, monuments, routes: Vec::new(), restrictions: Default::default(),
            grid: false, no_build: false, map_font: None, background: MAP_BACKGROUND, palette: None,
        });
        app.generation.queue.push(QueueItem { origin: None,
                       id, label: "Edited map".into(), recipe: recipe.clone(), state: State::Ready,
            detail: String::new(),
        });
        app.generation.next_id = id + 1;
        app.show_tab(Tab::Generated(id));
        app.apply_tab_request();
        let render = |app: &mut Studio, events| {
            let mut output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1200., 720.))),
                events, focused: true, ..Default::default()
            }, |ui| {
                app.toolbar(ui);
                app.generated_map(ui, id);
            });
            output.textures_delta.clear();
            output
        };
        let _ = render(&mut app, vec![]);
        let button = |pos, pressed| egui::Event::PointerButton {
            pos, pressed, button: egui::PointerButton::Primary, modifiers: Default::default(),
        };
        let output = render(&mut app, vec![]);
        let map = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Mesh(mesh) if mesh.texture_id == texture_id => Some(mesh.calc_bounds()),
            _ => None,
        }).unwrap();
        for (index, (target, delta)) in recipe.targets.iter().zip([
            egui::vec2(-30., 20.), egui::vec2(30., 10.), egui::vec2(6., 38.),
        ]).enumerate() {
            let revision = app.generation.revision;
            let start = edit_screen(map, recipe.size, target.x, target.z);
            let end = start + delta;
            let _ = render(&mut app, vec![egui::Event::PointerMoved(start), button(start, true)]);
            let wheel = egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0., 10.),
                phase: egui::TouchPhase::Move, modifiers: Default::default(),
            };
            if index == 1 {
                let _ = render(&mut app, vec![egui::Event::PointerMoved(end), wheel, button(end, false)]);
            } else {
                let _ = render(&mut app, vec![egui::Event::PointerMoved(end), wheel]);
                assert_eq!(app.generation.revision, revision, "A drag in progress is not a saved edit");
                let _ = render(&mut app, vec![button(end, false)]);
            }
            assert_eq!(app.generation.results[0].zoom, 1.);
            assert_eq!(app.generation.revision, revision + 1);
            assert!(app.dirty());
            assert!(app.task.is_none() && app.placement_task.is_none() && !app.generation.busy());
            assert_eq!(app.generation.results[0].recipe, recipe);
        }
        let staged = app.generation.edits[&id].poses.clone();
        let _ = render(&mut app, vec![history_key(egui::Key::Z, false)]);
        assert_eq!(app.generation.edits[&id].poses.len(), 2);
        assert_eq!(app.document_recipe(), &working);
        let _ = render(&mut app, vec![history_key(egui::Key::Y, false)]);
        let edit = &app.generation.edits[&id];
        assert_eq!(edit.poses, staged);
        assert_eq!(edit.poses.len(), 3);
        assert_eq!(edit.selected_monument, Some(2));
        assert!(edit.poses.values().all(|pose| pose[2] == 45.));
        let expected = edit.recipe(&recipe, &app.generation.results[0].monuments).unwrap();
        assert_eq!(expected.terrain_key(), recipe.terrain_key());
        app.generate_recipe(&ctx);
        let active_job = app.generation.run.as_ref().unwrap().active_id;
        let next_id = app.generation.next_id;
        app.generation.next_id = u64::MAX;
        app.apply_map_edit(id, &ctx);
        let edit = &app.generation.edits[&id];
        assert_eq!(edit.apply_error.as_deref(), Some("Map tab identifiers are exhausted"));
        assert_eq!(edit.poses, staged);
        assert!(edit.job.is_none());
        assert_eq!(app.generation.run.as_ref().unwrap().active_id, active_job);
        app.generation.next_id = next_id;
        let output = render(&mut app, vec![]);
        let apply = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(label) if label.galley.job.text == "Apply changes" => {
                Some(label.pos + label.galley.size() * 0.5)
            }
            _ => None,
        }).expect("The map header displays Apply changes");
        let _ = render(&mut app, vec![egui::Event::PointerMoved(apply), button(apply, true)]);
        let _ = render(&mut app, vec![button(apply, false)]);
        let job = app.generation.edits[&id].job.unwrap();
        assert!(app.generation.edits[&id].apply_error.is_none());
        assert!(!app.can_step_map_edit(id, false));
        assert_eq!(app.generation.run.as_ref().unwrap().active_id, active_job);
        let queued_edit = app.generation.queue.iter().find(|item| item.id == job).unwrap();
        assert_eq!(queued_edit.state, State::Waiting);
        assert_eq!(queued_edit.recipe, expected);
        let cancel_rebuild = |app: &mut Studio| {
            let center = |output: egui::FullOutput, text| {
                output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Text(label) if label.galley.job.text == text => {
                        Some(label.pos + label.galley.size() * 0.5)
                    }
                    _ => None,
                }).unwrap_or_else(|| panic!("Missing {text}"))
            };
            let map = center(render(app, vec![]), "Map");
            render(app, vec![egui::Event::PointerMoved(map), button(map, true)]);
            render(app, vec![button(map, false)]);
            let output = render(app, vec![]);
            let cancel = center(output, "Cancel rebuild");
            render(app, vec![egui::Event::PointerMoved(cancel), button(cancel, true)]);
            render(app, vec![button(cancel, false)]);
        };
        cancel_rebuild(&mut app);
        assert_eq!(app.generation.edits[&id].poses.len(), 3);
        assert!(app.generation.edits[&id].job.is_none());
        assert_eq!(app.generation.run.as_ref().unwrap().active_id, active_job);
        assert!(app.generation.queue.iter().all(|item| item.id != job));
        app.cancel_generation();
        app.apply_map_edit(id, &ctx);
        let cancelled = app.generation.edits[&id].job.unwrap();
        cancel_rebuild(&mut app);
        assert!(app.generation.edits[&id].job.is_none());
        assert_eq!(app.generation.edits[&id].poses, staged);
        assert!(app.generation.queue.iter().any(|item| item.id == cancelled && item.state == State::Cancelled));
        assert_eq!(app.visible_generated_tab(), Some(id));
        app.apply_map_edit(id, &ctx);
        let failed = app.generation.edits[&id].job.unwrap();
        app.fail_generation("Road fitting failed".into(), &ctx);
        let edit = &app.generation.edits[&id];
        assert_eq!(edit.poses, staged);
        assert!(edit.job.is_none());
        assert_eq!(edit.last_issue.as_ref().unwrap().id, failed);
        assert_eq!(app.generation.results[0].recipe, recipe);
        assert_eq!(app.visible_generated_tab(), Some(id));
        app.generation.discard_queue_prefix(app.generation.history_len());
        assert!(app.generation.queue.is_empty());
        assert_eq!(app.generation.edits[&id].last_issue.as_ref().unwrap().detail, "Road fitting failed");
        app.apply_map_edit(id, &ctx);
        assert!(app.generation.edits[&id].last_issue.is_none());
        assert!(app.task.is_none(), "Matching terrain should be reused");
        assert_eq!(app.generation.active_recipe(), Some(&expected));
        assert_eq!(app.visible_generated_tab(), Some(id));
        assert_eq!(app.document_recipe(), &working);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while app.generation.busy() && std::time::Instant::now() < deadline {
            app.poll(&ctx);
            app.poll_placement(&ctx);
            app.tick_generation(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(!app.generation.busy(), "{}", app.status);
        let result_card = app.generation.queue.iter().find(|item| item.id == id).unwrap();
        assert_eq!(result_card.state, State::Ready, "{}", app.status);
        assert_eq!(app.generation.results.len(), 1);
        assert_eq!(app.generation.results[0].id, id);
        assert_eq!(app.generation.results[0].recipe, expected);
        assert_eq!(app.visible_generated_tab(), Some(id));
        assert!(app.generation.edits.is_empty());
        assert_eq!(app.project, working);

        let result = &app.generation.results[0];
        let texture = result.image.as_ref().unwrap().as_ref().unwrap().id();
        let point = |map, monument: &map_monuments::Monument| {
            edit_screen(map, expected.size, monument.position[0], monument.position[2])
        };
        let output = render(&mut app, vec![]);
        let map = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Mesh(mesh) if mesh.texture_id == texture => Some(mesh.calc_bounds()),
            _ => None,
        }).expect("The rebuilt map has a preview texture");
        let monuments = &app.generation.results[0].monuments;
        let marker = point(map, &monuments[0]);
        let path = monuments[0].prefab.clone().expect("The placed monument has a prefab");
        let empty = [-0.3, 0.3, 0.].into_iter()
            .map(|fraction| map.center() + Vec2::new(map.width() * fraction, 0.))
            .find(|position| monuments.iter().all(|monument| {
                position.distance(point(map, monument)) > MONUMENT_PICK_RADIUS
            })).expect("Test map has space away from the markers");
        for point in [empty, marker] {
            egui::DragAndDrop::set_payload(&ctx, MonumentTemplate {
                map: id,
                name: "Catalog monument".into(),
                prefab: path.clone(),
            });
            let source = Pos2::new(2., 2.);
            let _ = render(&mut app, vec![egui::Event::PointerMoved(source), button(source, true)]);
            let _ = render(&mut app, vec![egui::Event::PointerMoved(point)]);
            let _ = render(&mut app, vec![button(point, false)]);
        }
        let draft = &app.generation.edits[&id].monuments;
        assert_eq!(draft.additions.len(), 2);
        assert!(draft.excluded(0));
        assert!((1..app.generation.results[0].monuments.len()).all(|index| !draft.excluded(index)));
        assert!(!app.generation.busy(), "Dropping a monument only stages an edit");

        let marker = point(map, &app.generation.results[0].monuments[1]);
        let _ = render(&mut app, vec![egui::Event::PointerMoved(marker), button(marker, true)]);
        let _ = render(&mut app, vec![button(marker, false)]);
        let output = render(&mut app, vec![]);
        let action = |label: &str| output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == label => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            _ => None,
        });
        assert!(action("Replace…").is_some(), "A marker click shows replacement");
        let remove = action("Remove").expect("A marker click shows removal");
        let _ = render(&mut app, vec![egui::Event::PointerMoved(remove), button(remove, true)]);
        let _ = render(&mut app, vec![button(remove, false)]);
        assert!(app.generation.edits[&id].monuments.excluded(1));
        assert!(!app.generation.edits[&id].monuments.excluded(2));
        assert!(!app.generation.busy(), "Removal stays staged until Apply changes");
    }

    #[test]
    fn generation_tabs_open_when_queued_and_only_the_active_job_draws_live_frames() {
        assert_eq!(elapsed_time(Duration::from_secs(5)), "0:05");
        assert_eq!(elapsed_time(Duration::from_secs(3661)), "1:01:01");
        let ctx = egui::Context::default();
        workbench_style::install(&ctx);
        let mut app = super::super::tests::fixture(&ctx);
        let mut batch = BatchPlan::exact(app.project.clone());
        batch.count = 2;
        batch.random_master = true;
        app.begin_generation(batch, &ctx);
        app.apply_tab_request();
        let active = app.generation.queue[0].id;
        let waiting = app.generation.queue[1].id;
        assert_eq!(app.visible_generated_tab(), Some(active));
        assert!(app.dock.find_tab(&Tab::Generated(waiting)).is_some());
        assert!(app.generation.results.is_empty());
        assert!(app.generation.open_maps(&app.dock).is_empty());
        assert!(app.live_job_tab_visible());
        app.generation.started = Some(Instant::now() - Duration::from_secs(73));
        app.stage_image = Some(egui::ColorImage::filled([32, 32], Color32::GREEN));

        let textures = std::cell::RefCell::new(snapshot::Textures::default());
        let render = |app: &mut Studio, id, width| {
            let mut output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(width, 480.))),
                ..Default::default()
            }, |ui| app.generated_map(ui, id));
            textures.borrow_mut().update(&output.textures_delta);
            output.textures_delta.clear();
            output
        };
        let label = |output: &egui::FullOutput, value| output.shapes.iter().any(|shape| {
            matches!(&shape.shape, egui::Shape::Text(label) if label.galley.job.text == value)
        });
        app.task.take();
        let token = Arc::new(AtomicBool::new(false));
        app.composition.seed_test_running(&app.project, token.clone());
        let erosion = mapper_studio::loading_progress::begin("Simulating erosion", Some(token));
        let reporter = erosion.reporter();
        reporter.update("", 128, Some(512), mapper_studio::loading_progress::Unit::Items);
        let progress_strip = |output: &egui::FullOutput| output.shapes.iter().find_map(|shape| {
            match &shape.shape {
                egui::Shape::Rect(r) if r.fill == workbench_style::PRIMARY_ACTION
                    && r.rect.height() == 3. => Some(r.rect),
                _ => None,
            }
        });
        for width in [205., 640.] {
            let started = app.generation.started.unwrap();
            let before = started.elapsed().as_secs();
            let output = render(&mut app, active, width);
            let after = started.elapsed().as_secs();
            assert!(label(&output, "Live"));
            let progress = progress_strip(&output).expect("Stage progress is visible");
            assert!((progress.width() - width * 0.25).abs() < 0.5);
            assert!(progress.top() < 40.);
            let id = app.stage_texture.as_ref().unwrap().id();
            let uses_live_texture = |output: &egui::FullOutput| output.shapes.iter()
                .any(|shape| matches!(&shape.shape, egui::Shape::Mesh(mesh)
                    if mesh.texture_id == id && shape.clip_rect.intersect(mesh.calc_bounds()).height() > 100.));
            assert!(uses_live_texture(&output));
            assert!(output.shapes.iter().any(|shape| {
                matches!(&shape.shape, egui::Shape::Text(text)
                    if text.pos.y < 35. && (before..=after)
                        .any(|seconds| text.galley.job.text == elapsed_time(Duration::from_secs(seconds))))
            }));
            if let Some(directory) = std::env::var_os("MAPPER_MAP_TABS_SNAPSHOTS") {
                let image = textures.borrow().raster(&ctx, output, [width as usize, 480]);
                let path = PathBuf::from(directory).join(format!("pending-generation-{width}.png"));
                mapper_studio::image_export::write_png(&path, &image, || false).unwrap();
            }
            let queued = render(&mut app, waiting, width);
            assert!(label(&queued, "Queued"));
            assert!(label(&queued, "Seeds"));
            assert!(label(&queued, "Up next"));
            assert!(label(&queued, "Simulating erosion"));
            assert!(!uses_live_texture(&queued));
            assert!(progress_strip(&queued).is_none());
        }
        reporter.stage("Placing cliffs");
        assert!(progress_strip(&render(&mut app, active, 640.)).is_none());
        app.cancel_queued_map(waiting, &ctx);
        app.apply_tab_request();
        assert!(app.dock.find_tab(&Tab::Generated(waiting)).is_none());
        app.cancel_generation();
        assert!(app.generation.elapsed[&active] >= Duration::from_secs(73));
        assert!(label(&render(&mut app, active, 640.), "Cancelled"));
    }

    #[test]
    fn waiting_map_can_move_next_and_open_the_running_viewport() {
        let ctx = egui::Context::default();
        workbench_style::install(&ctx);
        let mut app = super::super::tests::fixture(&ctx);
        let mut batch = BatchPlan::exact(app.project.clone());
        batch.count = 3;
        batch.random_master = true;
        app.begin_generation(batch, &ctx);
        let ids: Vec<_> = app.generation.queue.iter().map(|item| item.id).collect();
        let running = app.task.as_ref().unwrap().cancel.clone();
        let size = [205, 260];
        let mut textures = snapshot::Textures::default();
        let mut render = |app: &mut Studio, events| {
            let mut output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(size[0] as f32, size[1] as f32))),
                focused: true,
                events,
                ..Default::default()
            }, |ui| app.generated_map(ui, ids[2]));
            textures.update(&output.textures_delta);
            output.textures_delta.clear();
            output
        };
        let label_position = |output: &egui::FullOutput, caption: &str| {
            output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == caption => {
                    let bounds = text.galley.rect.translate(text.pos.to_vec2());
                    shape.clip_rect.contains_rect(bounds).then_some(bounds.center())
                }
                _ => None,
            })
        };
        let click = |point| [true, false].map(|pressed| vec![
            egui::Event::PointerMoved(point),
            egui::Event::PointerButton { pos: point, button: egui::PointerButton::Primary,
                pressed, modifiers: egui::Modifiers::NONE },
        ]);
        render(&mut app, vec![]);
        let waiting = render(&mut app, vec![]);
        assert!(label_position(&waiting, "2 maps ahead").is_some());
        for events in click(label_position(&waiting, "Run next").unwrap()) {
            render(&mut app, events);
        }
        let promoted = render(&mut app, vec![]);
        assert!(label_position(&promoted, "Up next").is_some());
        assert!(label_position(&promoted, "Run next").is_none());
        let active_button = label_position(&promoted, "Open running map").unwrap();
        for events in click(active_button) {
            render(&mut app, events);
        }
        assert_eq!(app.requested_tab, Some(Tab::Generated(ids[0])));
        app.apply_tab_request();
        assert_eq!(app.visible_generated_tab(), Some(ids[0]));
        assert_eq!(app.generation.queue.iter().map(|item| item.id).collect::<Vec<_>>(), [ids[0], ids[2], ids[1]]);
        assert!(!running.load(Ordering::Relaxed));
        if let Some(directory) = std::env::var_os("MAPPER_WAITING_MAP_SNAPSHOTS") {
            for (name, output) in [("waiting-map.png", waiting), ("waiting-map-next.png", promoted)] {
                let image = textures.raster(&ctx, output, size);
                mapper_studio::image_export::write_png(&PathBuf::from(&directory).join(name), &image, || false).unwrap();
            }
        }
        app.cancel_generation();
    }

    #[test]
    fn failed_map_tab_exposes_recovery_and_keeps_the_report_collapsed() {
        let detail = format!("Rail bed exceeds cumulative cut/fill allowance\n{}", "Grade witness. ".repeat(310));
        for (theme, width) in [(egui::Theme::Dark, 205), (egui::Theme::Light, 640)] {
            let ctx = egui::Context::default();
            workbench_style::install(&ctx);
            ctx.set_theme(theme);
            ctx.global_style_mut(|style| style.animation_time = 0.);
            let mut app = super::super::tests::fixture(&ctx);
            let mut recipe = app.project.clone();
            recipe.size += 100;
            app.generation.queue.push(QueueItem { origin: None,
                id: 1,
                label: "Seed 24680 · 1".into(),
                recipe: recipe.clone(),
                state: State::Failed,
                detail: detail.clone(),
            });
            let textures = std::cell::RefCell::new(snapshot::Textures::default());
            let render = |app: &mut Studio, events, queue| {
                let mut output = ctx.run_ui(egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(width as f32, 350.))),
                    events,
                    focused: true,
                    ..Default::default()
                }, |ui| {
                    if queue {
                        app.queue(ui);
                    } else {
                        app.generated_map(ui, 1);
                    }
                });
                textures.borrow_mut().update(&output.textures_delta);
                output.textures_delta.clear();
                output
            };
            let control = |output: &egui::FullOutput, name: &str| {
                output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.job.text == name => {
                        let bounds = text.galley.rect.translate(text.pos.to_vec2());
                        shape.clip_rect.contains_rect(bounds).then(|| bounds.center())
                    }
                    _ => None,
                }).unwrap_or_else(|| panic!("Missing visible control {name} at {width}px"))
            };
            let has_witness = |output: &egui::FullOutput| output.shapes.iter().any(|shape| {
                matches!(&shape.shape, egui::Shape::Text(label)
                    if label.galley.job.text.contains("Grade witness."))
            });
            let click = |app: &mut Studio, pos, queue| {
                for pressed in [true, false] {
                    render(app, vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton {
                        pos, pressed, button: egui::PointerButton::Primary, modifiers: egui::Modifiers::NONE,
                    }], queue);
                }
            };

            render(&mut app, vec![], true);
            let queue = render(&mut app, vec![], true);
            // The title is truncated in the narrow queue. Verify reopening here
            // at the full width and recovery at both widths.
            if width == 640 {
                click(&mut app, control(&queue, "Seed 24680 · 1"), true);
                app.apply_tab_request();
                assert_eq!(app.visible_generated_tab(), Some(1));
            }
            render(&mut app, vec![], false);
            let collapsed = render(&mut app, vec![], false);
            assert!(!has_witness(&collapsed));
            control(&collapsed, diagnostic_ui::summary(&detail).as_ref());
            control(&collapsed, "Save recipe…");
            let technical = control(&collapsed, "Technical report");
            let capture = |output, variant| {
                if let Some(directory) = std::env::var_os("MAPPER_MAP_TABS_SNAPSHOTS") {
                    let image = textures.borrow().raster(&ctx, output, [width, 350]);
                    let path = PathBuf::from(directory).join(format!("failed-map-{width}-{variant}.png"));
                    mapper_studio::image_export::write_png(&path, &image, || false).unwrap();
                }
            };
            capture(collapsed, "collapsed");
            click(&mut app, technical, false);
            let expanded = render(&mut app, vec![], false);
            assert!(has_witness(&expanded));
            control(&expanded, "Copy full details");
            let edit = control(&expanded, "Edit recipe");
            capture(expanded, "expanded");
            click(&mut app, edit, false);
            assert_eq!(app.project, recipe);
            assert_eq!(app.generation.recipe_recovery, Some(diagnostic_ui::RecipeSection::Rails));
        }
    }

    #[test]
    fn batch_runs_sequentially_retains_exact_recipes_and_stops_cleanly() {
        let ctx = egui::Context::default();
        let mut app = super::super::tests::fixture(&ctx);
        let original = app.project.clone();
        let plan = BatchPlan {
            count: 2,
            nonce: 71,
            randomize: [false, false, false, false, true, false, false],
            ..BatchPlan::exact(app.project.clone())
        };
        let expected = plan.recipes().unwrap();
        app.begin_generation(plan, &ctx);
        assert_eq!(app.document_recipe(), &original);
        let save = tempfile::tempdir().unwrap();
        let path = save.path().join("batch.mapper");
        app.save_project_to(path.clone(), &ctx).unwrap();
        let save_deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.project_io.busy() && std::time::Instant::now() < save_deadline {
            app.poll_project_io(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(Project::load(&path).unwrap(), original);
        assert_eq!(expected[0].terrain_key(), expected[1].terrain_key());
        let mut first_terrain_texture = None;
        // The resource pool serializes this job with concurrent image tests.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        while app.generation.busy() && std::time::Instant::now() < deadline {
            app.poll(&ctx);
            if app.generation.run.as_ref().is_some_and(|run| run.finished == 0 && run.terrain_ready) {
                first_terrain_texture = app.texture.as_ref().map(egui::TextureHandle::id);
            }
            app.poll_placement(&ctx);
            app.tick_generation(&ctx);
            app.apply_tab_request();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(!app.generation.busy(), "{}", app.status);
        assert!(first_terrain_texture.is_some());
        assert_eq!(app.texture.as_ref().map(egui::TextureHandle::id), first_terrain_texture);
        assert_eq!(app.project, original);
        assert_eq!(app.generation.results.len(), 2, "{}", app.status);
        assert_eq!(
            app.generation
                .results
                .iter()
                .map(|r| r.recipe.clone())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(app.generation.queue.iter().all(|q| q.state == State::Ready));
        assert_eq!(app.visible_generated_tab(), Some(app.generation.results[0].id));
        assert!(app.dock.find_tab(&Tab::Generated(app.generation.results[1].id)).is_some());
        assert_ne!(
            app.generation.results[0].label,
            app.generation.results[1].label
        );
        let size = app.generation.results[0].image.as_ref().unwrap().as_ref().unwrap().size();
        for result in &app.generation.results {
            let tab = Tab::Generated(result.id);
            assert!(app.dock.find_tab(&tab).is_some());
            assert_eq!(result.image.as_ref().unwrap().as_ref().unwrap().size(), size);
        }
        let id = app.generation.results[0].id;
        let texture = app.generation.results[0].image.as_ref().unwrap().as_ref().unwrap().id();
        let path = app.dock.find_tab(&Tab::Generated(id)).unwrap();
        assert!(app.dock.find_tab(&Tab::Home).is_some());
        assert_eq!(path.node, app.map_tab_path().unwrap().node);
        app.dock.remove_tab(path);
        app.show_tab(Tab::Generated(id));
        app.apply_tab_request();
        assert!(app.dock.find_tab(&Tab::Generated(id)).is_some());
        assert_eq!(texture, app.generation.results[0].image.as_ref().unwrap().as_ref().unwrap().id());
        assert!(app.task.is_none());
        assert!(app.placement_task.is_none());
        let current = app.project.clone();
        app.begin_generation(
            BatchPlan {
                count: 3,
                nonce: 72,
                randomize: [false, false, false, false, true, false, false],
                ..BatchPlan::exact(current)
            },
            &ctx,
        );
        app.cancel_generation();
        assert_eq!(app.project, original);
        assert!(!app.generation.busy());
        assert!(app.task.is_none());
        assert!(app.placement_task.is_none());
        assert_eq!(app.generation.results.len(), 2);
        assert!(app.generation.queue[..2].iter().all(|item| item.state == State::Ready));
        assert!(app.generation.queue[2..].iter().all(|item| item.state == State::Cancelled));
    }
    #[test]
    fn ordered_generation_publishes_its_first_layout_from_composition() {
        let ctx = egui::Context::default();
        let mut app = super::super::tests::fixture(&ctx);
        let mut recipe = app.project.clone();
        let initial = mapper_studio::placement_preview::solve(
            &recipe, &[17000; 65 * 65], 65, || false,
        ).unwrap().unwrap();
        recipe.preview_engine = mapper_studio::native_preview::VERSION.into();
        recipe.composition_engine =
            Some(papyrus_terrain::monument_composition::SOURCE_HEIGHT_VERSION.into());
        recipe.monument_rules = Some(serde_json::from_value(serde_json::json!({
            "engine": mapper_studio::monument_source::VERSION,
            "source_sha256": "a".repeat(64),
            "surface_modifiers": mapper_studio::monument_source::SOURCE_PRIORITY_VERSION,
        })).unwrap());

        app.begin_generation(BatchPlan::exact(app.project.clone()), &ctx);
        app.task = None;
        app.project = recipe.clone();
        app.generation.queue[0].recipe = recipe.clone();
        let preview = app.preview.as_mut().unwrap();
        preview.key = recipe.terrain_key();
        preview.ordered_terrain = true;

        assert!(!app.deferred_generation_placement());
        app.generation_terrain_ready();
        app.start_placement(&ctx);
        assert!(app.placement_task.is_none());
        assert!(app.current_layout().is_none());
        app.composition.seed_test_completed(&ctx, &recipe, None, initial.clone(), true);
        app.publish_composed_layout();
        assert!(app.current_layout().is_none());
        app.composition.seed_test_completed(&ctx, &recipe, None, initial.clone(), false);
        app.publish_composed_layout();
        let accepted = app.current_layout().unwrap();
        assert_eq!(accepted.recipe, recipe);
        assert_eq!(
            mapper_studio::composition::layout_fingerprint(&accepted.solution),
            mapper_studio::composition::layout_fingerprint(&initial),
        );
        assert_eq!(app.composition.completion(&recipe), Ok(true));

        app.cancel_generation();
        assert_eq!(app.project, Project::default());
        app.project = recipe.clone();
        app.layouts.clear();
        app.active_layout = None;
        app.composition.seed_test_completed(&ctx, &recipe, None, initial, false);
        app.publish_composed_layout();
        assert!(app.current_layout().is_none());
        assert!(app.composition.view(&recipe).is_none());
    }

    #[test]
    fn failed_queue_item_retains_its_recipe_and_remaining_maps_finish() {
        let ctx = egui::Context::default();
        let mut app = super::super::tests::fixture(&ctx);
        let original = app.project.clone();
        let data = tempfile::tempdir().unwrap();
        let settings = data.path().join("Mapper").join("settings.json");
        app.settings_path = Some(settings.clone());
        let plan = BatchPlan {
            count: 3,
            randomize: [false, false, false, false, true, false, false],
            ..BatchPlan::exact(app.project.clone())
        };
        let recipes = plan.recipes().unwrap();
        app.begin_generation(plan, &ctx);
        let old_task = app.task.as_ref().unwrap().cancel.clone();
        let reason = "Road generation rejected a steep approach";
        app.fail_generation(reason.into(), &ctx);
        assert!(old_task.load(Ordering::Relaxed));
        assert!(app.generation.busy());
        assert_eq!(app.generation.queue[0].state, State::Failed);
        assert_eq!(app.generation.queue[0].detail, reason);
        assert_eq!(app.generation.queue[0].recipe, recipes[0]);
        assert_eq!(app.generation.queue[1].state, State::Running);
        assert_eq!(app.generation.queue[2].state, State::Waiting);
        assert_eq!(app.generation.run.as_ref().unwrap().finished, 1);
        let failed_id = app.generation.queue[0].id;
        app.load_queue_recipe(failed_id);
        assert_eq!(app.project, recipes[1]);
        assert_eq!(app.document_recipe(), &recipes[0]);
        assert!(app.generation.results.is_empty());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        while app.generation.busy() && std::time::Instant::now() < deadline {
            app.poll(&ctx);
            app.poll_placement(&ctx);
            app.tick_generation(&ctx);
            app.apply_tab_request();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(!app.generation.busy(), "{}", app.status);
        assert!(app.generation.queue[1..].iter().all(|q| q.state == State::Ready));
        assert_eq!(app.generation.queue[0].detail, reason);
        assert_eq!(app.generation.results.iter().map(|r| r.recipe.clone()).collect::<Vec<_>>(), recipes[1..]);
        assert_eq!(app.status, "2 ready · 1 failed.");
        let path = failed_recipe_path(Some(&settings)).unwrap();
        let retained = mapper_studio::profile::Profile::load_snapshot(&path).unwrap();
        assert_eq!(retained.recipe, recipes[0]);
        assert_eq!(app.project, recipes[0]);
        assert!(app.task.is_none());
        assert!(app.placement_task.is_none());
        app.load_queue_recipe(failed_id);
        assert_eq!(app.project, recipes[0]);
        assert_eq!(app.requested_tab, Some(Tab::Recipe(FIRST_RECIPE)));
        assert!(!app.generation_in_progress());
        assert_eq!(app.generation.queue[0].detail, reason);
        assert_eq!(app.generation.results.len(), 2);
        app.history.undo(&mut app.project);
        assert_eq!(app.project, original);
        app.begin_generation(BatchPlan::exact(app.project.clone()), &ctx);
        assert_eq!(app.generation.queue[0].id, failed_id);
        assert_eq!(app.generation.queue[0].state, State::Failed);
        assert_eq!(app.generation.queue[0].detail, reason);
        assert_eq!(app.generation.queue.last().unwrap().state, State::Running);
        assert_eq!(mapper_studio::profile::Profile::load_snapshot(&path).unwrap(), retained);
        app.cancel_generation();
    }
    #[test]
    fn cancelling_after_a_failure_preserves_that_failure_and_stops_the_rest() {
        let ctx = egui::Context::default();
        let mut app = super::super::tests::fixture(&ctx);
        app.begin_generation(BatchPlan {
            count: 3, randomize: [false, false, false, false, true, false, false],
            ..BatchPlan::exact(app.project.clone())
        }, &ctx);
        app.fail_generation("First map failed".into(), &ctx);
        let next = app.task.as_ref().unwrap().cancel.clone();
        app.cancel_generation();
        assert!(next.load(Ordering::Relaxed));
        assert!(!app.generation.busy());
        assert_eq!(app.generation.queue[0].state, State::Failed);
        assert_eq!(app.generation.queue[0].detail, "First map failed");
        assert!(app.generation.queue[1..].iter().all(|q| q.state == State::Cancelled));
        assert!(app.task.is_none());
        assert!(app.placement_task.is_none());
    }

    #[test]
    fn editing_a_routing_failure_reveals_its_recipe_section() {
        for (stage, report, section, body) in [
            (
                "roads",
                "Composition unavailable: Bounded complete road layouts exhausted: {\"attempts\":[]}",
                "Roads & trails",
                "Roads require native terrain",
            ),
            (
                "rails",
                "Composition unavailable: Rail bed exceeds cumulative cut/fill allowance",
                "Rails",
                "Rails require native terrain",
            ),
        ] {
            for dark in [true, false] {
                let ctx = egui::Context::default();
                ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
                workbench_style::install(&ctx);
                ctx.global_style_mut(|style| {
                    style.animation_time = 0.;
                    style.scroll_animation = egui::style::ScrollAnimation::none();
                });
                let mut app = super::super::tests::fixture(&ctx);
                let recipe = app.project.clone();
                app.generation.queue.push(QueueItem { origin: None,
                    id: 1,
                    label: "Seed 24680".into(),
                    recipe: recipe.clone(),
                    state: State::Failed,
                    detail: report.into(),
                });
                let frame = |app: &mut Studio| ctx.run_ui(egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(290., 195.))),
                    ..Default::default()
                }, |ui| { egui::CentralPanel::default().show(ui, |ui| app.recipe(ui)); });
                let visible = |output: &egui::FullOutput, label: &str| {
                    output.shapes.iter().any(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.job.text == label => {
                            shape.clip_rect.contains(text.pos + text.galley.size() * 0.5)
                        }
                        _ => false,
                    })
                };

                let mut textures = snapshot::Textures::default();
                let mut initial = frame(&mut app);
                textures.update(&initial.textures_delta);
                initial.textures_delta.clear();
                assert!(!visible(&initial, body));
                app.load_queue_recipe(1);
                assert_eq!(app.requested_tab, Some(Tab::Recipe(FIRST_RECIPE)));
                for _ in 0..4 {
                    let mut opened = frame(&mut app);
                    textures.update(&opened.textures_delta);
                    opened.textures_delta.clear();
                }
                let mut focused = frame(&mut app);
                textures.update(&focused.textures_delta);
                focused.textures_delta.clear();
                assert!(visible(&focused, section), "Section not visible: {section}");
                assert!(visible(&focused, body), "Section body not visible: {body}");
                assert!(app.generation.recipe_recovery.is_none());
                assert_eq!(app.project, recipe);
                assert_eq!(app.generation.queue[0].detail, report);

                if let Some(directory) = std::env::var_os("MAPPER_FAILURE_UI_SNAPSHOTS") {
                    let image = textures.raster(&ctx, focused, [290, 195]);
                    let theme = if dark { "dark" } else { "light" };
                    mapper_studio::image_export::write_png(
                        &PathBuf::from(directory).join(format!("recipe-recovery-{stage}-{theme}.png")),
                        &image,
                        || false,
                    ).unwrap();
                }
            }
        }
    }
    #[test]
    fn a_new_run_retains_failed_recipes_and_counts_only_its_own_outcomes() {
        let ctx = egui::Context::default();
        let mut app = super::super::tests::fixture(&ctx);
        let failed = app.project.clone();
        app.generate_recipe(&ctx);
        let old_id = app.generation.run.as_ref().unwrap().active_id;
        app.fail_generation("First run failed".into(), &ctx);
        app.project.size += 100;
        let current = app.project.clone();
        app.generate_recipe(&ctx);
        let run = app.generation.run.as_ref().unwrap();
        assert_eq!((run.start, run.finished), (1, 1));
        assert_eq!(app.generation.history_len(), 1);
        assert_eq!(app.generation.active_recipe(), Some(&current));
        assert_eq!(app.generation.queue[0].detail, "First run failed");

        app.load_queue_recipe(old_id);
        assert_eq!(app.document_recipe(), &failed);
        assert_eq!(app.generation.active_recipe(), Some(&current));
        app.fail_generation("Second run failed".into(), &ctx);
        assert_eq!(app.status, "Second run failed");
        assert_eq!(app.generation.queue.len(), 2);
        assert_eq!(app.generation.queue[0].detail, "First run failed");
        assert_eq!(app.generation.queue[1].detail, "Second run failed");
    }

    #[test]
    fn queue_history_yields_to_new_jobs_and_can_be_cleared_during_a_run() {
        let ctx = egui::Context::default();
        workbench_style::install(&ctx);
        ctx.global_style_mut(|style| style.animation_time = 0.);
        let mut app = super::super::tests::fixture(&ctx);
        for id in 1..=MAX_BATCH as u64 {
            app.generation.queue.push(QueueItem { origin: None,
                id,
                label: format!("Map {id}"),
                recipe: app.project.clone(),
                state: State::Failed,
                detail: "An earlier failure".into(),
            });
        }
        app.generation.next_id = MAX_BATCH as u64 + 1;
        app.begin_generation(BatchPlan {
            count: 2,
            randomize: [false, false, false, false, true, false, false],
            ..BatchPlan::exact(app.project.clone())
        }, &ctx);
        assert_eq!(app.generation.queue.len(), MAX_BATCH);
        assert_eq!(app.generation.queue[0].id, 3);
        assert_eq!(app.generation.available_slots(), MAX_BATCH - 2);

        let textures = std::cell::RefCell::new(snapshot::Textures::default());
        let render = |app: &mut Studio, events| {
            let mut output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(460., 300.))),
                events,
                focused: true,
                ..Default::default()
            }, |ui| {
                egui::CentralPanel::default().show(ui, |ui| app.queue(ui));
            });
            textures.borrow_mut().update(&output.textures_delta);
            output.textures_delta.clear();
            output
        };
        let position = |output: &egui::FullOutput, name: &str| {
            output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == name => {
                    let rect = text.galley.rect.translate(text.pos.to_vec2());
                    shape.clip_rect.contains_rect(rect).then(|| rect.center())
                }
                _ => None,
            }).unwrap_or_else(|| panic!("Missing visible control: {name}"))
        };
        let click = |app: &mut Studio, pos| {
            for pressed in [true, false] {
                let _ = render(app, vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton {
                    pos, pressed, button: egui::PointerButton::Primary, modifiers: Default::default(),
                }]);
            }
        };
        let _ = render(&mut app, vec![]);
        let details = position(&render(&mut app, vec![]), "Actions");
        click(&mut app, details);
        let menu = render(&mut app, vec![]);
        let clear = position(&menu, "Clear queue history");
        if let Some(directory) = std::env::var_os("MAPPER_MAP_TABS_SNAPSHOTS") {
            let image = textures.borrow().raster(&ctx, menu, [460, 300]);
            mapper_studio::image_export::write_png(
                &PathBuf::from(directory).join("queue-history.png"), &image, || false,
            ).unwrap();
        }
        click(&mut app, clear);

        let run = app.generation.run.as_ref().unwrap();
        assert_eq!((run.start, run.finished), (0, 0));
        assert_eq!(app.generation.queue.len(), 2);
        assert_eq!(app.generation.queue[0].id, run.active_id);
        assert_eq!(app.generation.queue[1].state, State::Waiting);
        assert!(app.generation.revealed_job.is_none_or(|id| id == run.active_id));
        app.cancel_generation();
    }

    #[test]
    fn every_failed_item_reaches_a_terminal_state_without_capturing_a_map() {
        let ctx = egui::Context::default();
        let mut app = super::super::tests::fixture(&ctx);
        app.begin_generation(BatchPlan {
            count: 3, randomize: [false, false, false, false, true, false, false],
            ..BatchPlan::exact(app.project.clone())
        }, &ctx);
        for index in 0..3 {
            app.fail_generation(format!("Map {index} failed"), &ctx);
            assert_eq!(app.generation.queue[index].detail, format!("Map {index} failed"));
            assert_eq!(app.generation.busy(), index < 2);
        }
        assert!(app.generation.queue.iter().all(|q| q.state == State::Failed));
        assert!(app.generation.results.is_empty());
        assert!(app.task.is_none());
        assert_eq!(app.status, "3 failed.");
    }
    #[test]
    fn navigating_between_split_maps_updates_the_active_map() {
        let ctx = egui::Context::default();
        let mut app = super::super::tests::fixture(&ctx);
        let central = Tab::Generated(1);
        let split = Tab::Generated(2);
        app.show_tab(central);
        app.apply_tab_request();
        assert!(app.dock.find_tab(&Tab::Home).is_some());
        let anchor = app.dock.find_tab(&central).unwrap();
        app.dock.main_surface_mut().split_right(anchor.node, 0.5, vec![split]);

        for (tab, id) in [(split, 2), (central, 1), (split, 2)] {
            app.show_tab(tab);
            app.apply_tab_request();
            assert_eq!(app.visible_generated_tab(), Some(id));
        }
        app.show_tab(Tab::Home);
        app.apply_tab_request();
        assert_eq!(app.visible_generated_tab(), None);
    }

    #[test]
    fn queued_navigation_survives_dock_drawing_and_uses_the_viewport_leaf() {
        let ctx = egui::Context::default();
        let mut app = super::super::tests::fixture(&ctx);
        let dock = std::mem::replace(&mut app.dock, DockState::new(vec![]));
        app.show_tab(Tab::Generated(71));
        app.dock = dock;
        app.apply_tab_request();
        let generated = app.dock.find_tab(&Tab::Generated(71)).unwrap();
        assert!(app.dock.find_tab(&Tab::Home).is_some());
        let map = app.map_tab_path().unwrap();
        assert_eq!(generated.node, map.node);
        assert_eq!(generated.surface, map.surface);
        app.show_tab(Tab::Queue);
        app.apply_tab_request();
        assert!(app.dock.find_tab(&Tab::Queue).is_some());
    }
    #[test]
    fn queue_issue_popover_saves_its_recipe_and_copies_the_full_report() {
        let ctx = egui::Context::default();
        diagnostic_ui::set_support_environment(&ctx, diagnostic_ui::SupportEnvironment {
            version: "0.1.0-beta.1".into(),
            installation: None,
            profile: None,
        });
        let mut app = super::super::tests::fixture(&ctx);
        let _installation = require_matching_rust_installation(&mut app);
        let expected_hash = app.project.installation.as_ref().unwrap().native.sha256.clone();
        workbench_style::install(&ctx);
        ctx.global_style_mut(|style| style.animation_time = 0.);
        let report = format!(
            "Composition unavailable: Connected rail fixed heights conflict with terrain/earthwork bounds at track 2, control 0\nRail recovery details: {}",
            "Detailed constraint. ".repeat(250),
        );
        let item = std::cell::RefCell::new(QueueItem { origin: None,
            id: 1,
            label: "Seed 24680 · 3".into(),
            recipe: app.project.clone(),
            state: State::Incomplete,
            detail: report.clone(),
        });
        let can_edit = std::cell::Cell::new(true);
        let action = std::cell::Cell::new(None);
        let textures = std::cell::RefCell::new(snapshot::Textures::default());
        let render = |events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(420., 310.))),
                    events,
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        ui.horizontal_top(|ui| {
                            action.set(queue_table::queue_table_actions(ui, &item.borrow(), false, false, false, can_edit.get(), true, 140.));
                        });
                    });
                },
            );
            textures.borrow_mut().update(&output.textures_delta);
            output.textures_delta.clear();
            output
        };
        let capture = |output, file| {
            if let Some(directory) = std::env::var_os("MAPPER_MAP_TABS_SNAPSHOTS") {
                let image = textures.borrow().raster(&ctx, output, [420, 310]);
                let path = PathBuf::from(directory).join(file);
                mapper_studio::image_export::write_png(&path, &image, || false).unwrap();
            }
        };
        let has_report = |output: &egui::FullOutput| {
            output.shapes.iter().any(|shape| {
                matches!(&shape.shape, egui::Shape::Text(label)
                    if label.galley.job.text.contains("Detailed constraint."))
            })
        };
        let position = |output: &egui::FullOutput, text: &str| {
            output.shapes.iter().find_map(|shape| {
                if let egui::Shape::Text(label) = &shape.shape
                    && label.galley.job.text == text
                {
                    let rect = label.galley.rect.translate(label.pos.to_vec2());
                    return shape.clip_rect.contains_rect(rect).then(|| rect.center());
                }
                None
            }).unwrap_or_else(|| panic!("Missing visible control: {text}"))
        };
        let click = |pos| {
            let event = |pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            render(vec![egui::Event::PointerMoved(pos), event(true)]);
            render(vec![event(false)])
        };
        render(vec![]);
        let before = render(vec![]);
        click(position(&before, "View issues"));
        let after = render(vec![]);
        position(&after, "Open map");
        position(
            &after,
            "Surface rails couldn’t connect all required entrances. Try another terrain seed or review Rail settings.",
        );
        let technical_report = position(&after, "Technical report");
        assert!(!has_report(&after));
        capture(after, "queue-issues.png");
        click(technical_report);
        let expanded = render(vec![]);
        assert!(has_report(&expanded));
        let save_recipe = position(&expanded, "Save recipe…");
        capture(expanded, "queue-issues-expanded.png");
        click(save_recipe);
        assert_eq!(action.get(), Some(QueueAction::SaveRecipe(1)));
        let closed = render(vec![]);
        click(position(&closed, "View issues"));
        let after = render(vec![]);
        let copied = click(position(&after, "Copy full details"));
        assert!(copied.platform_output.commands.iter().any(|command| {
            matches!(command, egui::OutputCommand::CopyText(value) if value == &report)
        }));
        let support = click(position(&render(vec![]), "Copy error message"));
        let copied = support.platform_output.commands.iter().find_map(|command| {
            match command {
                egui::OutputCommand::CopyText(value) => Some(value),
                _ => None,
            }
        }).expect("the error action writes to the clipboard");
        let (message, code) = copied.split_once("\n\nMapper support code\n").unwrap();
        assert_eq!(message, report);
        let decoded = mapper_studio::support_snapshot::Snapshot::decode(code).unwrap();
        let decoded = serde_json::to_value(decoded).unwrap();
        assert_eq!(decoded["mapper_version"], "0.1.0-beta.1");
        assert_eq!(decoded["rust"]["native_sha256"], expected_hash);

        click(position(&render(vec![]), "Edit recipe"));
        assert_eq!(action.get(), Some(QueueAction::LoadRecipe(1)));

        item.borrow_mut().state = State::Failed;
        let failed = render(vec![]);
        capture(failed.clone(), "queue-failed.png");
        click(position(&failed, "View issues"));
        can_edit.set(false);
        click(position(&render(vec![]), "Edit recipe"));
        assert_eq!(action.get(), None);
        can_edit.set(true);
        click(position(&render(vec![]), "Edit recipe"));
        assert_eq!(action.get(), Some(QueueAction::LoadRecipe(1)));
        click(position(&render(vec![]), "View issues"));
        click(position(&render(vec![]), "Retry"));
        assert_eq!(action.get(), Some(QueueAction::Requeue(1)));
    }

    #[test]
    fn retry_from_failed_map_queues_the_saved_recipe_and_keeps_the_report() {
        let ctx = egui::Context::default();
        workbench_style::install(&ctx);
        let mut app = super::super::tests::fixture(&ctx);
        let draft = app.project.clone();
        let plan = BatchPlan {
            count: 2,
            nonce: 119,
            randomize: [false, false, false, false, true, false, false],
            ..BatchPlan::exact(draft.clone())
        };
        let recipes = plan.recipes().unwrap();
        app.begin_generation(plan, &ctx);
        let id = app.generation.queue[0].id;
        let detail = "Map preview worker stopped";
        app.fail_generation(detail.into(), &ctx);
        let active = app.task.as_ref().unwrap().cancel.clone();
        let size = [300, 210];
        let mut textures = snapshot::Textures::default();
        let mut render = |app: &mut Studio, events| {
            let mut output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(size[0] as f32, size[1] as f32))),
                focused: true,
                events,
                ..Default::default()
            }, |ui| app.generated_map(ui, id));
            textures.update(&output.textures_delta);
            output.textures_delta.clear();
            output
        };
        render(&mut app, vec![]);
        let before = render(&mut app, vec![]);
        let retry = before.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == "Retry" => {
                let bounds = text.galley.rect.translate(text.pos.to_vec2());
                shape.clip_rect.contains_rect(bounds).then_some(bounds.center())
            }
            _ => None,
        }).expect("Failed map should offer Retry");
        for pressed in [true, false] {
            render(&mut app, vec![
                egui::Event::PointerMoved(retry),
                egui::Event::PointerButton { pos: retry, button: egui::PointerButton::Primary,
                    pressed, modifiers: egui::Modifiers::NONE },
            ]);
        }
        assert_eq!(app.generation.queue.len(), 3);
        assert_eq!(app.generation.queue[0].id, id);
        assert_eq!(app.generation.queue[0].state, State::Failed);
        assert_eq!(app.generation.queue[0].detail, detail);
        assert_eq!(app.generation.active_recipe(), Some(&recipes[1]));
        assert!(!active.load(Ordering::Relaxed));
        let queued = &app.generation.queue[2];
        assert_eq!(queued.state, State::Waiting);
        assert_eq!(queued.recipe, recipes[0]);
        assert_ne!(queued.label, app.generation.queue[0].label);
        assert_eq!(app.requested_tab, Some(Tab::Generated(queued.id)));
        assert_eq!(app.document_recipe(), &draft);
        if let Some(directory) = std::env::var_os("MAPPER_QUEUE_RETRY_SNAPSHOTS") {
            let image = textures.raster(&ctx, before, size);
            let path = PathBuf::from(directory).join("failed-map-retry.png");
            mapper_studio::image_export::write_png(&path, &image, || false).unwrap();
        }
        app.cancel_generation();
    }

    #[test]
    fn queue_buttons_remove_waiting_maps_and_cancel_only_the_active_map() {
        let ctx = egui::Context::default();
        workbench_style::install(&ctx);
        workbench_style::set_reduced_motion(&ctx, true);
        let mut app = super::super::tests::fixture(&ctx);
        let original = app.project.clone();
        let plan = BatchPlan {
            count: 3,
            nonce: 71,
            randomize: [false, false, false, false, true, false, false],
            ..BatchPlan::exact(original.clone())
        };
        let recipes = plan.recipes().unwrap();
        app.begin_generation(plan, &ctx);
        let ids: Vec<_> = app.generation.queue.iter().map(|item| item.id).collect();
        let first = app.task.as_ref().unwrap().cancel.clone();
        let moves = [(0, [10., 20., 30.])].into();
        app.generation.edits.insert(99, MapEdit {
            poses: moves,
            job: Some(ids[1]),
            ..Default::default()
        });

        let render = |app: &mut Studio, events| {
            let mut output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(610., 145.))),
                events,
                ..Default::default()
            }, |ui| {
                egui::CentralPanel::default().show(ui, |ui| app.queue(ui));
            });
            output.textures_delta.clear();
            output
        };
        let click = |app: &mut Studio, text: &str| {
            let frame = render(app, vec![]);
            let position = frame.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Text(label) if label.galley.job.text == text => {
                    let bounds = label.galley.rect.translate(label.pos.to_vec2());
                    shape.clip_rect.contains_rect(bounds).then_some(bounds.center())
                }
                _ => None,
            }).unwrap_or_else(|| panic!("Missing Queue action {text}"));
            for pressed in [true, false] {
                let _ = render(app, vec![
                    egui::Event::PointerMoved(position),
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ]);
            }
        };

        let _ = render(&mut app, vec![]);
        assert!(!app.generation.can_enqueue(MAX_BATCH - 2));
        app.cancel_queued_map(ids[1], &ctx);
        assert!(app.generation.can_enqueue(MAX_BATCH - 2));
        assert_eq!(app.generation.queue.iter().map(|item| item.id).collect::<Vec<_>>(), [ids[0], ids[2]]);
        assert!(!first.load(Ordering::Relaxed));
        assert!(app.generation.edits[&99].job.is_none());
        assert_eq!(app.generation.edits[&99].poses[&0], [10., 20., 30.]);
        assert_eq!(app.document_recipe(), &original);

        click(&mut app, "Cancel");
        assert!(first.load(Ordering::Relaxed));
        assert_eq!(app.generation.queue[0].state, State::Cancelled);
        assert_eq!(app.generation.queue[1].state, State::Running);
        assert_eq!(app.generation.active_recipe(), Some(&recipes[2]));
        assert_eq!(app.document_recipe(), &original);
        let last = app.task.as_ref().unwrap().cancel.clone();

        click(&mut app, "Cancel");
        assert!(last.load(Ordering::Relaxed));
        assert!(!app.generation.busy());
        assert!(app.generation.queue.iter().all(|item| item.state == State::Cancelled));
        assert_eq!(app.status, "2 cancelled.");
        assert_eq!(app.project, original);

        let plan = BatchPlan {
            count: 3,
            nonce: 73,
            randomize: [false, false, false, false, true, false, false],
            ..BatchPlan::exact(original.clone())
        };
        app.begin_generation(plan, &ctx);
        app.finish_map_tab_requests();
        let run = app.generation.run.as_ref().unwrap();
        let active = run.active_id;
        let active_recipe = app.project.clone();
        let waiting: Vec<_> = app.generation.queue[run.finished + 1..]
            .iter().map(|item| item.id).collect();
        assert_eq!(waiting.len(), 2);
        for &id in &waiting {
            assert!(app.dock.find_tab(&Tab::Generated(id)).is_some());
        }
        app.generation.edits.get_mut(&99).unwrap().job = Some(waiting[0]);
        app.generation.queue_focus = Some(waiting[1]);
        app.generation.queue_filter = QueueFilter::Issues;
        let running = app.task.as_ref().unwrap().cancel.clone();
        let revision = app.generation.revision;
        click(&mut app, "Actions");
        click(&mut app, "Remove waiting maps (2)");
        app.finish_map_tab_requests();
        assert!(!running.load(Ordering::Relaxed));
        assert_eq!(app.generation.active_recipe(), Some(&active_recipe));
        assert_eq!(app.generation.available_slots(), MAX_BATCH - 1);
        assert_eq!(app.generation.revision, revision + 1);
        assert_eq!(app.generation.queue_filter, QueueFilter::Issues);
        assert!(app.generation.queue_focus.is_none());
        assert_eq!(app.generation.queue.iter().map(|item| item.id).collect::<Vec<_>>(),
            [ids[0], ids[2], active]);
        assert!(app.dock.find_tab(&Tab::Generated(active)).is_some());
        for id in waiting {
            assert!(app.dock.find_tab(&Tab::Generated(id)).is_none());
        }
        assert!(app.generation.edits[&99].job.is_none());
        assert_eq!(app.generation.edits[&99].poses[&0], [10., 20., 30.]);
        assert_eq!(app.document_recipe(), &original);
        app.cancel_generation();
    }

    #[test]
    fn run_next_promotes_a_waiting_map_and_keeps_the_active_recipe() {
        let ctx = egui::Context::default();
        workbench_style::install(&ctx);
        let mut app = super::super::tests::fixture(&ctx);
        let original = app.project.clone();
        let plan = BatchPlan {
            count: 4,
            nonce: 97,
            randomize: [false, false, false, false, true, false, false],
            ..BatchPlan::exact(original.clone())
        };
        let recipes = plan.recipes().unwrap();
        app.begin_generation(plan, &ctx);
        let ids: Vec<_> = app.generation.queue.iter().map(|item| item.id).collect();
        let cancel = app.task.as_ref().unwrap().cancel.clone();
        let order = |app: &Studio| app.generation.queue.iter().map(|item| item.id).collect::<Vec<_>>();
        app.run_queued_map_next(ids[3], &ctx);
        assert_eq!(order(&app), [ids[0], ids[3], ids[1], ids[2]]);
        assert_eq!(app.generation.active_recipe(), Some(&recipes[0]));
        assert_eq!(app.generation.queue[1].recipe, recipes[3]);
        assert!(!cancel.load(Ordering::Relaxed));
        assert_eq!(app.document_recipe(), &original);
        assert_eq!(app.generation.queue_focus, Some(ids[3]));

        app.run_queued_map_next(ids[0], &ctx);
        assert_eq!(order(&app), [ids[0], ids[3], ids[1], ids[2]]);
        app.cancel_queued_map(ids[0], &ctx);
        assert!(cancel.load(Ordering::Relaxed));
        assert_eq!(app.generation.active_recipe(), Some(&recipes[3]));
        assert_eq!(app.generation.queue[1].state, State::Running);
        assert_eq!(order(&app), [ids[0], ids[3], ids[1], ids[2]]);
    }

    #[test]
    fn queue_scrolls_later_rows_into_view_with_the_mouse_wheel() {
        let ctx = egui::Context::default();
        workbench_style::install(&ctx);
        let mut app = super::super::tests::fixture(&ctx);
        app.generation.queue = (0..5).map(|id| QueueItem { origin: None,
            id,
            label: format!("Seed 24680 · {id}"),
            recipe: Project::default(),
            state: if id == 4 { State::Cancelled } else { State::Waiting },
            detail: String::new(),
        }).collect();
        let mut render = |events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(390., 145.))),
                    events,
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| app.queue(ui));
                },
            );
            output.textures_delta.clear();
            output.shapes.iter().any(|shape| {
                if let egui::Shape::Text(label) = &shape.shape
                    && label.galley.job.text == "Cancelled"
                {
                    return shape.clip_rect.contains_rect(label.galley.rect.translate(label.pos.to_vec2()));
                }
                false
            })
        };
        render(vec![]);
        assert!(!render(vec![]));
        let scrolled = render(vec![
            egui::Event::PointerMoved(egui::pos2(125., 100.)),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                phase: egui::TouchPhase::Move,
                delta: egui::vec2(0., -1800.),
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        assert!(scrolled || (0..8).any(|_| render(vec![])));
    }

    #[test]
    fn queue_filter_preserves_job_order_and_locate_reveals_a_hidden_runner() {
        let ctx = egui::Context::default();
        workbench_style::install(&ctx);
        ctx.all_styles_mut(|style| style.animation_time = 0.);
        let mut app = super::super::tests::fixture(&ctx);
        let recipe = app.project.clone();
        let states = [State::Ready, State::Running, State::Waiting, State::Waiting,
            State::Failed, State::Incomplete, State::Cancelled];
        app.generation.queue = states.into_iter().enumerate().map(|(i, state)| QueueItem { origin: None,
            id: i as u64 + 1,
            label: format!("Seed {}", i + 1),
            recipe: recipe.clone(),
            state,
            detail: String::new(),
        }).collect();
        app.generation.run = Some(Run {
            active_id: 2, start: 0, finished: 1, template: None, terrain_ready: false,
        });
        app.generation.queue[4].detail = "Composition unavailable: Rail bed exceeds cumulative cut/fill allowance\n{\"stage\":\"rails\"}".into();
        app.generation.queue[5].detail = "Two required monuments missing".into();
        let revision = app.generation.revision;
        let size = std::cell::Cell::new([860, 245]);
        let mut textures = snapshot::Textures::default();
        let mut render = |app: &mut Studio, events| {
            let [width, height] = size.get();
            let mut output = ctx.run_ui(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(width as f32, height as f32))),
                focused: true, events, ..Default::default()
            }, |ui| app.queue(ui));
            textures.update(&output.textures_delta);
            output.textures_delta.clear();
            output
        };
        let visible = |output: &egui::FullOutput, caption: &str| {
            output.shapes.iter().filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == caption => {
                    let center = text.pos + text.galley.rect.center().to_vec2();
                    shape.clip_rect.contains(center).then_some(center)
                }
                _ => None,
            }).last()
        };
        let click = |point| [true, false].map(|pressed| vec![
            egui::Event::PointerMoved(point),
            egui::Event::PointerButton {
                pos: point, button: egui::PointerButton::Primary,
                pressed, modifiers: egui::Modifiers::NONE,
            },
        ]);
        render(&mut app, vec![]);
        for (caption, choice) in [("All jobs", QueueFilter::Active), ("Active", QueueFilter::Issues)] {
            let output = render(&mut app, vec![]);
            for event in click(visible(&output, caption).unwrap()) { render(&mut app, event); }
            let menu = render(&mut app, vec![]);
            for event in click(visible(&menu, choice.label()).unwrap()) { render(&mut app, event); }
            let filtered = render(&mut app, vec![]);
            assert_eq!(app.generation.queue_filter, choice);
            assert!(visible(&filtered, "1").is_none());
            if choice == QueueFilter::Active {
                assert!(visible(&filtered, "2").is_some());
                assert!(visible(&filtered, "Next").is_some());
                assert!(visible(&filtered, "5").is_none());
            } else {
                assert!(visible(&filtered, "5").is_some());
                assert!(visible(&filtered, "6").is_some());
                assert!(visible(&filtered, "View issues").is_some());
                assert!(visible(&filtered, "Rails need more grading").is_some());
                assert!(visible(&filtered, "Two required monuments missing").is_some());
                assert!(visible(&filtered, "{\"stage\":\"rails\"}").is_none());
                assert!(visible(&filtered, "2").is_none());
            }
        }
        let issues = render(&mut app, vec![]);
        for event in click(visible(&issues, "Actions").unwrap()) { render(&mut app, event); }
        let menu = render(&mut app, vec![]);
        for event in click(visible(&menu, "Locate running map").unwrap()) { render(&mut app, event); }
        assert_eq!(app.generation.queue_filter, QueueFilter::All);
        let located = render(&mut app, vec![]);
        assert!(visible(&located, "2").is_some());

        assert_eq!(app.generation.revision, revision);
        assert!(app.generation.queue.iter().map(|item| item.id).eq(1..=states.len() as u64));
        assert_eq!(app.generation.queue.iter().map(|item| item.state).collect::<Vec<_>>(), states);
        assert_eq!(app.project, recipe);
    }

    #[test]
    fn queue_and_viewport_tabs_render_without_thumbnail_gallery_or_recipe_mutation() {
        for dark in [false, true] {
            for size in [[1050, 720], [1440, 940]] {
                let ctx = egui::Context::default();
                ctx.set_visuals(if dark {
                    egui::Visuals::dark()
                } else {
                    egui::Visuals::light()
                });
                workbench_style::install(&ctx);
                let mut app = super::super::tests::fixture(&ctx);
                let recipe = app.project.clone();
                let image = if let Some(path) = std::env::var_os("MAPPER_WORKSPACE_OVERVIEW") {
                    let file = std::io::BufReader::new(std::fs::File::open(path).unwrap());
                    let mut reader = png::Decoder::new(file).read_info().unwrap();
                    assert!(reader.info().width <= 4096 && reader.info().height <= 4096);
                    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
                    let info = reader.next_frame(&mut pixels).unwrap();
                    let size = [info.width as usize, info.height as usize];
                    assert_eq!(info.bit_depth, png::BitDepth::Eight);
                    match info.color_type {
                        png::ColorType::Rgb => egui::ColorImage::from_rgb(size, &pixels[..info.buffer_size()]),
                        png::ColorType::Rgba => egui::ColorImage::from_rgba_unmultiplied(size, &pixels[..info.buffer_size()]),
                        _ => panic!("Workspace overview must be an RGB or RGBA PNG"),
                    }
                } else {
                    egui::ColorImage::filled([256, 256], Color32::from_rgb(74, 92, 52))
                };
                app.generation.results.push(Candidate {
                    identity:None,
                    overview:None,
                    archive:None,archive_issue:String::new(),
                    id: 1,
                    label: "Seed 24680".into(),
                    recipe: recipe.clone(),
                    image: Some(Ok(ctx.load_texture(
                        "test-complete-map",
                        image,
                        egui::TextureOptions::LINEAR,
                    ))),
                    zoom: 1.,
                    pan: Vec2::ZERO,
                    placed: 2,
                    missing: 0,
                    routes: Vec::new(),
                    monuments: [
                        ("Large Oil Rig", [-2150., 0., 900.]),
                        ("Rail tunnel entrance", [30., 15., 100.]),
                    ]
                    .into_iter()
                    .enumerate()
                    .map(|(index, (name, position))| map_monuments::Monument {
                        id: 100 + index as u64,
                        name: name.into(),
                        prefab: None,
                        position,
                        yaw: 0.,
                        restriction_origin: None,
                    })
                    .collect(),
                    restrictions: Default::default(),
                    grid: false,
                    no_build: false,
                    map_font: None,
                    background: MAP_BACKGROUND,
                    palette: None,
                });
                app.generation.queue = [
                    State::Ready,
                    State::Running,
                    State::Incomplete,
                    State::Failed,
                    State::Cancelled,
                ]
                .into_iter()
                .enumerate()
                .map(|(i, state)| QueueItem { origin: None,
                    id: i as u64 + 1,
                    label: format!("Seed 24680 · {}", i + 1),
                    recipe: recipe.clone(),
                    state,
                    detail: if state == State::Incomplete {
                        "Requested scenery output is missing".into()
                    } else if state == State::Failed {
                        "Road constraints rejected this map".into()
                    } else {
                        String::new()
                    },
                })
                .collect();
                app.generation.run = Some(Run {
                    active_id: 2,
                    template: Some(recipe.clone()),
                    start: 0,
                    finished: 1,
                    terrain_ready: false,
                });
                app.generation.revealed_job = Some(2);
                let (_terrain_sender, receiver) = std::sync::mpsc::channel();
                app.task = Some(Task {
                    ordered_terrain: false,
                    key: recipe.terrain_key(),
                    receiver,
                    cancel: Default::default(),
                    progress: Default::default(),
                });
                app.generation_progress = Some("Loading saved terrain");
                let (stage, fraction) = app.generation_stage();
                assert_eq!(stage, "Loading saved terrain");
                assert!(fraction.is_none());
                app.show_tab(Tab::Generated(1));
                app.apply_tab_request();
                let mut textures = snapshot::Textures::default();
                let mut final_output = None;
                for _ in 0..3 {
                    let mut output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(
                                Pos2::ZERO,
                                egui::vec2(size[0] as f32, size[1] as f32),
                            )),
                            ..Default::default()
                        },
                        |ui| {
                            let snapshot = app.visible_generated_tab();
                            let mut dock = std::mem::replace(&mut app.dock, DockState::new(vec![]));
                            egui::Panel::top("toolbar")
                                .frame(egui::Frame::NONE.fill(ui.visuals().extreme_bg_color).inner_margin(egui::Margin::symmetric(6, 3)))
                                .show(ui, |ui| app.toolbar(ui));
                            egui::CentralPanel::default().frame(workbench_style::dock_frame(ui)).show(ui, |ui| {
                                let mut tabs = Tabs::new(&mut app, snapshot, &dock);
                                DockArea::new(&mut dock).style(workbench_style::dock(ui))
                                    .show_leaf_close_all_buttons(false).show_leaf_collapse_buttons(false)
                                    .show_inside(ui, &mut tabs);
                                tabs.finish(&mut dock);
                            });
                            app.dock = dock;
                            app.apply_tab_request();
                        },
                    );
                    textures.update(&output.textures_delta);
                    output.textures_delta.clear();
                    final_output = Some(output);
                }
                assert_eq!(app.project, recipe);
                let bounds=|tab|app.dock.find_tab(&tab).and_then(|p|app.dock.leaf(p.node_path()).ok()).map(|leaf|leaf.rect).unwrap();
                assert!(bounds(Tab::Recipe(FIRST_RECIPE)).width()<size[0] as f32*0.3);
                assert!(bounds(Tab::Generated(1)).width()>size[0] as f32*0.4);
                assert!(bounds(Tab::Outliner).left()>bounds(Tab::Generated(1)).left());
                assert_eq!(app.generation.results[0].zoom, 1.);
                let output = final_output.unwrap();
                let text = output
                    .shapes
                    .iter()
                    .filter_map(|s| {
                        if let egui::Shape::Text(t) = &s.shape {
                            Some(t.galley.job.text.clone())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                for action in ["Start all", "Pause all", "All jobs", "Actions"] {
                    assert!(output.shapes.iter().any(|shape| {
                        if let egui::Shape::Text(label) = &shape.shape
                            && label.galley.job.text == action
                        {
                            let rect = label.galley.rect.translate(label.pos.to_vec2());
                            return shape.clip_rect.contains_rect(rect);
                        }
                        false
                    }), "Missing visible queue action {action} at {size:?}: {:?}\n{text}",
                        output.shapes.iter().filter_map(|shape| match &shape.shape {
                            egui::Shape::Text(label) if label.galley.job.text == action => {
                                Some((label.galley.rect.translate(label.pos.to_vec2()), shape.clip_rect))
                            }
                            _ => None,
                        }).collect::<Vec<_>>());
                }
                assert!(text.contains("Queue"), "{text}");
                assert!(text.lines().any(|line| line == "Placed"), "{text}");
                assert!(text.lines().any(|line| line == "Catalog"), "{text}");
                assert!(text.contains("24680"), "{text}");
                assert!(text.contains("Loading saved terrain"), "{text}");
                assert!(text.lines().any(|line| line == "Actions"), "{text}");
                let queue = bounds(Tab::Queue);
                assert!(!output.shapes.iter().any(|shape| match &shape.shape {
                    egui::Shape::Text(label) => label.galley.job.text.ends_with('%')
                        && queue.contains(label.pos),
                    _ => false,
                }), "An indeterminate queue stage must not show a percentage");
                assert!(text.contains("4000 m · 2 monuments"), "{text}");
                assert_eq!(text.lines().filter(|line| *line == "Seeds").count(), 1, "{text}");
                assert!(text.contains("Large Oil Rig"), "{text}");
                assert!(text.contains("Rail tunnel entrance"), "{text}");
                assert!(!text.lines().any(|line| line == "Edit map"), "{text}");
                assert!(!text.contains("Map preview"));
                assert!(!text.contains("Recipe candidates"));
                assert!(!text.contains("Show Working map"));
                assert!(!text.contains("Recipe preview"));
                assert!(!text.contains("Preferred location"));
                assert!(!text.contains("Generated candidates"));
                assert!(!text.contains("Activity"));
                if let Some(directory) = std::env::var_os("MAPPER_MAP_TABS_SNAPSHOTS") {
                    let path = PathBuf::from(directory).join(format!(
                        "map-tabs-{}-{}x{}.png",
                        if dark { "dark" } else { "light" },
                        size[0],
                        size[1]
                    ));
                    let image = textures.raster(&ctx, output, [size[0] as usize, size[1] as usize]);
                    mapper_studio::image_export::write_png(&path, &image, || false).unwrap();
                }
            }
        }
    }
    #[test]
    fn batch_popup_keeps_checkboxes_open_and_queues_new_master_seeds() {
        let ctx = egui::Context::default();
        workbench_style::install(&ctx);
        ctx.style_mut_of(ctx.theme(), |style| style.animation_time = 0.);
        let mut app = super::super::tests::fixture(&ctx);
        let original = app.project.clone();
        let render = |app: &mut Studio, events| {
            let mut frame = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1050., 720.))),
                    events,
                    ..Default::default()
                },
                |ui| {
                    app.recipe_generate(ui);
                },
            );
            frame.textures_delta.clear();
            frame
        };
        let text_rect = |frame: &egui::FullOutput, label: &str| {
            frame.shapes.iter().find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.job.text == label => {
                    Some(t.galley.rect.translate(t.pos.to_vec2()))
                }
                _ => None,
            })
        };
        let click = |app: &mut Studio, pos| {
            for pressed in [true, false] {
                render(app, vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos, button: egui::PointerButton::Primary, pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ]);
            }
            render(app, vec![])
        };
        let frame = render(&mut app, vec![]);
        let generate = text_rect(&frame, "Generate").unwrap().center();
        let split_button = frame.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Rect(rect) if rect.brush.is_some() && rect.rect.contains(generate) => Some(rect.rect),
            _ => None,
        }).expect("Generate has a visible split-button background");
        let dropdown = egui::pos2(split_button.right() - 17., generate.y);
        let menu = click(&mut app, dropdown);
        assert!(text_rect(&menu, "Batch generation").is_some());
        assert!(text_rect(&menu, "Queue 4").is_some());
        assert!(text_rect(&menu, "Run 4").is_some());
        let advanced = text_rect(&menu, "Vary stage seeds").unwrap();
        assert!(advanced.bottom() < generate.y);
        click(&mut app, advanced.center());
        let mut menu = render(&mut app, vec![]);
        let terrain = text_rect(&menu, "Terrain").unwrap().center();
        for expected in [true, false] {
            menu = click(&mut app, terrain);
            assert_eq!(app.generation.randomize[0], expected);
            assert!(text_rect(&menu, "Terrain").is_some());
        }
        assert_eq!(app.project, original);
        click(&mut app, text_rect(&menu, "Queue 4").unwrap().center());
        assert_eq!(app.generation.queue.len(), 4);
        let mut masters = std::collections::BTreeSet::from([original.master_seed]);
        for item in &app.generation.queue {
            assert!(masters.insert(item.recipe.master_seed));
            let mut expected = original.clone();
            expected.master_seed = item.recipe.master_seed;
            expected.refresh_inherited_seeds();
            assert_eq!(item.recipe, expected);
        }
        app.begin_generation(BatchPlan {
            count: MAX_BATCH - 4,
            nonce: 93,
            random_master: true,
            ..BatchPlan::exact(original.clone())
        }, &ctx);
        assert_eq!(app.generation.queue.len(), MAX_BATCH);
        assert_eq!(app.generation_readiness(), Err(GenerationBlocker::QueueFull));
        let active = render(&mut app, vec![]);
        let generate = text_rect(&active, "Queue").unwrap().center();
        let dropdown = egui::pos2(dropdown.x, generate.y);
        assert!(text_rect(&click(&mut app, generate), "Queue full").is_some());
        assert_eq!(app.generation.queue.len(), MAX_BATCH);

        let menu = click(&mut app, dropdown);
        assert!(text_rect(&menu, "Queue 4").is_some());
        let last_waiting = app.generation.queue.last().unwrap().id;
        app.cancel_queued_map(last_waiting, &ctx);
        let menu = render(&mut app, vec![]);
        assert!(text_rect(&menu, "1 slot available").is_some());
        click(&mut app, text_rect(&menu, "Queue 4").unwrap().center());
        assert_eq!(app.generation.queue.len(), MAX_BATCH - 1);
        assert!(app.generation_readiness().is_ok());

        click(&mut app, dropdown);
        click(&mut app, generate);
        assert_eq!(app.generation.queue.len(), MAX_BATCH);
        assert_eq!(app.generation.queue.last().unwrap().recipe, original);
        app.cancel_generation();
        assert_eq!(app.project, original);
    }

    #[test]
    fn generate_requires_a_seed_and_reuses_a_randomized_field_value() {
        let ctx = egui::Context::default();
        let mut app = super::super::tests::fixture(&ctx);
        assert_eq!(Studio::new_workbench_project().master_seed, 0);
        app.project.master_seed = 0;
        app.project.refresh_inherited_seeds();
        assert_eq!(app.generation_readiness(), Err(GenerationBlocker::MissingSeed));
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("draft.mapper");
        app.project.save(&path).unwrap();
        assert_eq!(Project::load(&path).unwrap(), app.project);
        assert!(BatchPlan::exact(app.project.clone()).recipes().is_err());
        app.generate_recipe(&ctx);
        assert!(app.generation.queue.is_empty());
        assert_eq!(app.status, mapper_studio::MASTER_SEED_REQUIRED);
        app.project.seeds[4].inherited = false;
        let fixed_seed = app.project.seeds[4];
        app.randomize_master_seed();
        let first_seed = app.project.master_seed;
        app.randomize_master_seed();
        assert_ne!(app.project.master_seed, first_seed);
        assert_eq!(app.project.seeds[4], fixed_seed);
        let original = app.project.clone();
        for _ in 0..2 {
            app.generate_recipe(&ctx);
            assert_eq!(app.generation.active_recipe(), Some(&original));
            app.cancel_generation();
            assert_eq!(app.project, original);
        }
        app.generate_recipe(&ctx);
        let mut replacement = original;
        replacement.size += 100;
        app.project = replacement.clone();
        app.tick_generation(&ctx);
        assert!(!app.generation.busy());
        assert_eq!(app.project, replacement);
    }

    #[test]
    fn stopping_randomized_generation_releases_only_a_mismatched_working_preview() {
        for candidate_preview in [false, true] {
            let ctx = egui::Context::default();
            let mut app = super::super::tests::fixture(&ctx);
            let original = app.project.clone();
            app.begin_generation(BatchPlan {
                nonce: 91,
                random_master: true,
                ..BatchPlan::exact(original.clone())
            }, &ctx);
            let preview = app.preview.as_mut().unwrap();
            if candidate_preview {
                preview.key = app.project.terrain_key();
            }
            let heights = Arc::downgrade(&preview.heights);
            assert_ne!(app.project.terrain_key(), original.terrain_key());
            app.cancel_generation();
            assert_eq!(app.project, original);
            assert_eq!(app.preview_current(), !candidate_preview);
            assert_eq!(heights.upgrade().is_none(), candidate_preview);
            assert_eq!(app.texture.is_none(), candidate_preview);
        }
    }

    #[test]
    fn disabled_generate_keeps_readable_text_in_light_mode() {
        let ctx = egui::Context::default();
        workbench_style::install(&ctx);
        ctx.set_theme(egui::Theme::Light);
        let mut app = super::super::tests::fixture(&ctx);
        let mut frame = ctx.run_ui(egui::RawInput::default(), |ui| {
            app.generate_split_button(ui, 260., false, Some(&GenerationBlocker::AssetsUnavailable), FIRST_RECIPE);
        });

        frame.textures_delta.clear();
        let text = frame.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == "Generate" => Some(text),
            _ => None,
        }).expect("disabled Generate label is painted");
        let colors = &ctx.style_of(egui::Theme::Light).visuals;
        let glyph = text.override_text_color.unwrap_or(text.galley.job.sections[0].format.color);
        assert_eq!(glyph, colors.weak_text_color());
        assert_eq!(text.opacity_factor, 1.);
        let luminance = |color: Color32| {
            let [r, g, b, _] = egui::Rgba::from(color).to_array();
            0.2126 * r + 0.7152 * g + 0.0722 * b
        };
        let contrast = (luminance(colors.widgets.noninteractive.weak_bg_fill) + 0.05)
            / (luminance(glyph) + 0.05);
        assert!(contrast >= 4.5, "Generate label contrast was {contrast:.2}:1");
    }

    #[test]
    fn workspace_keeps_its_height_and_generation_actions_work_in_their_panels() {
        let _background = mapper_studio::loading_progress::begin("Terrain colors", None);
        for dark in [false, true] {
            let ctx = egui::Context::default();
            ctx.set_visuals(if dark {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            });
            workbench_style::install(&ctx);
            let mut app = super::super::tests::fixture(&ctx);
            let mut rects = Vec::new();
            let mut textures = snapshot::Textures::default();
            let seed = app.project.master_seed;
            for state in ["idle", "missing-seed", "busy", "full", "blocked", "assets"] {
                app.project.master_seed = if state == "missing-seed" { 0 } else { seed };
                if matches!(state, "busy" | "full") {
                    let total = if state == "full" { MAX_BATCH } else { 2 };
                    app.generation.queue = (0..total)
                        .map(|index| QueueItem { origin: None,
                            id: index as u64 + 1,
                            label: format!("Seed {}", index + 1),
                            recipe: app.project.clone(),
                            state: if index == 0 { State::Running } else { State::Waiting },
                            detail: String::new(),
                        })
                        .collect();
                    app.generation.run = Some(Run {
                        active_id: 1,
                        template: None,
                        start: 0,
                        finished: 0,
                        terrain_ready: false,
                    });
                }
                let _installation = if matches!(state, "blocked" | "assets") {
                    app.generation.run = None;
                    let installation = require_matching_rust_installation(&mut app);
                    if state == "assets" {
                        select_rust_installation(&mut app, installation.path(), &ctx);
                    }
                    Some(installation)
                } else {
                    None
                };
                let input = |events| egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1050., 720.),
                    )),
                    events,
                    ..Default::default()
                };
                let mut render = |app: &mut Studio, events| {
                    let mut frame = ctx.run_ui(input(events), |ui| {
                        egui::Panel::left("test-recipe")
                            .exact_size(320.)
                            .show(ui, |ui| {
                                app.recipe_generate(ui);
                                app.recipe(ui);
                            });
                        egui::CentralPanel::default().show(ui, |ui| {
                            rects.push(ui.available_rect_before_wrap());
                            app.queue(ui);
                        });
                    });
                    textures.update(&frame.textures_delta);
                    frame.textures_delta.clear();
                    frame
                };
                render(&mut app, vec![]);
                let output = render(&mut app, vec![]);
                assert!(!output.shapes.iter().any(
                    |s| matches!(&s.shape, egui::Shape::Text(t) if t.galley.job.text == "Ready")
                ));
                let action = "Generate";
                assert!(output.shapes.iter().any(
                    |s| matches!(&s.shape,egui::Shape::Text(t) if t.galley.job.text == action)
                ));
                assert_eq!(output.shapes.iter().any(
                    |s| matches!(&s.shape,egui::Shape::Text(t) if t.galley.job.text=="Queue full")
                ), state == "full");
                let recovery = output.shapes.iter().find_map(|s| {
                    let egui::Shape::Text(text) = &s.shape else { return None };
                    let rect = text.galley.rect.translate(text.pos.to_vec2());
                    if rect.left() > 320. || rect.top() <= 630. || !s.clip_rect.contains_rect(rect) { return None; }
                    match text.galley.job.text.as_str() {
                        "Rust installations…" => Some(("blocked", rect.center())),
                        "Retry asset loading" => Some(("assets", rect.center())),
                        _ => None,
                    }
                });
                assert_eq!(recovery.map(|(state, _)| state),
                    matches!(state, "blocked" | "assets").then_some(state));
                if recovery.is_some() {
                    assert!(!output.shapes.iter().any(|s| matches!(&s.shape,
                        egui::Shape::Text(text)
                            if text.galley.job.text == "Preparing installation monument metadata…"
                    )));
                }
                let queue_action = |frame: &egui::FullOutput, label| frame.shapes.iter().find_map(|shape| {
                    let egui::Shape::Text(text) = &shape.shape else { return None };
                    let rect = text.galley.rect.translate(text.pos.to_vec2());
                    (text.galley.job.text == label && shape.clip_rect.contains_rect(rect))
                        .then_some(rect.center())
                });
                let click = |render: &mut dyn FnMut(&mut Studio, Vec<egui::Event>) -> egui::FullOutput,
                             app: &mut Studio, pos| {
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
                let actions = queue_action(&output, "Actions").unwrap();
                assert!(actions.x > 320., "Actions should be inside Queue");
                click(&mut render, &mut app, actions);
                let menu = render(&mut app, vec![]);
                let cancel = queue_action(&menu, "Cancel all");
                assert_eq!(cancel.is_some(), matches!(state, "busy" | "full"));
                if let Some(pos) = cancel {
                    click(&mut render, &mut app, pos);
                    assert!(!app.generation.busy());
                    assert!(app.generation.queue.iter().all(|job| job.state == State::Cancelled));
                } else {
                    click(&mut render, &mut app, actions);
                }
                if let Some((action, pos)) = recovery {
                    let recipe = app.project.clone();
                    app.settings_open = false;
                    app.settings_page = settings::Page::Appearance;
                    click(&mut render, &mut app, pos);
                    if action == "blocked" {
                        assert!(app.settings_open);
                        assert!(app.settings_page == settings::Page::Installations);
                    } else {
                        assert!(!app.settings_open);
                        assert!(app.prefabs.loading_assets());
                        assert_eq!(
                            app.generation_readiness().unwrap_err().message(),
                            "Loading installation assets…",
                        );
                    }
                    assert_eq!(app.project, recipe);
                }
            }
            assert!(rects.iter().all(|rect| *rect == rects[0]), "Workspace height changed");
        }
    }
}
