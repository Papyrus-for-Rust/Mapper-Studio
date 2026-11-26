use super::*;
use mapper_studio::project_bundle::{Source, load_workspace_selected};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    io::Read,
    sync::{
        Arc, Weak,
        atomic::AtomicBool,
        mpsc::{self, Receiver},
    },
};

pub(super) struct OverviewTask {
    key: OverviewKey,
    cancel: Arc<AtomicBool>,
    receiver: Receiver<Result<Option<Arc<tempfile::TempPath>>, String>>,
}
pub(super) struct OverviewKey {
    id: u64,
    image: Weak<egui::ColorImage>,
}
impl OverviewKey {
    fn matches(&self, other: &Self) -> bool {
        self.id == other.id && Weak::ptr_eq(&self.image, &other.image)
    }
}
pub(crate) struct Identity {
    layout: [u8; 32],
    world: Option<std::sync::Weak<papyrus_terrain::monument_composition::Composed>>,
    colors: mapper_studio::map_colors::Preferences,
}
impl Identity {
    pub(super) fn matches(&self, other: &Self) -> bool {
        self.layout == other.layout
            && self.colors == other.colors
            && match (&self.world, &other.world) {
                (Some(a), Some(b)) => std::sync::Weak::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            }
    }
}
impl Drop for OverviewTask {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
impl Studio {
    pub(crate) fn current_map_identity(&self) -> Option<Identity> {
        Some(Identity {
            layout: mapper_studio::composition::layout_fingerprint(
                &self.current_layout()?.solution,
            ),
            world: self
                .composition
                .view(&self.project)
                .map(|view| Arc::downgrade(&view.maps)),
            colors: self.preferences.map_colors.clone(),
        })
    }
    pub(crate) fn capture_project(&self) -> Result<Capture, String> {
        let mut capture = self.generation.capture(
            self.generation.open_maps(&self.dock),
            self.visible_generated_tab(),
        )?;
        if self.generation.busy() {
            return Ok(capture);
        }
        let Some(identity) = self.current_map_identity() else {
            return Ok(capture);
        };
        let Some((image, _)) = self.exportable_overview() else {
            return Ok(capture);
        };
        if let Some(index) = self.generation.results.iter().position(|result| {
            result.recipe == self.project
                && result
                    .identity
                    .as_ref()
                    .is_some_and(|saved| saved.matches(&identity))
        }) {
            if self.visible_generated_tab().is_none() {
                let saved = &mut capture.candidates[index].saved;
                saved.pan = [self.pan.x, self.pan.y];
                saved.zoom = self.zoom;
                saved.grid = self.grid;
                saved.no_build = self.show_no_build;
                capture.workspace.active = Some(saved.id);
                if !capture.workspace.open.contains(&saved.id) {
                    capture.workspace.open.push(saved.id);
                }
            }
            return Ok(capture);
        }
        let id = self.generation.next_id;
        if id == u64::MAX {
            return Err("Map tab identifiers are exhausted".into());
        }
        while capture.candidates.len() > MAX_BATCH {
            let Some(index) = capture.candidates.iter().position(|candidate| {
                Some(candidate.saved.id) != capture.workspace.active
                    && !capture.workspace.edits.iter().any(|edit| edit.map == candidate.saved.id)
            }) else {
                return Ok(capture);
            };
            let removed = capture.candidates.remove(index).saved.id;
            capture.workspace.open.retain(|&id| id != removed);
            capture.workspace.queue.retain(|item| item.id != removed);
        }
        let layout = self.current_layout().unwrap();
        let view = self.composition.view(&self.project);
        let mut monuments = map_monuments::initial(&self.project, &layout.solution);
        if let Some(view) = view {
            monuments.extend(view.generated_monuments.iter().cloned());
        }
        for monument in &mut monuments {
            monument.name = self
                .monument_icons
                .name(monument.prefab.as_deref(), &monument.name);
        }
        let (palette, repaint_overview) = view
            .and_then(|view| view.palette.as_ref().map(|palette| (view, palette)))
            .map_or((None, false), |(view, palette)| {
                let (styled, repaint) = overview_style(
                    palette,
                    &self.preferences.map_colors,
                    Arc::ptr_eq(image, &view.image),
                );
                (Some(Arc::new(styled)), repaint)
            });
        let native = view
            .map(|view| {
                Ok::<_, String>(WorkingWorld {
                    maps: view.maps.clone(),
                    solution: view.solution.clone(),
                    root: self
                        .source
                        .bound_root(&self.project)
                        .ok_or("Working map source is unavailable")?
                        .to_owned(),
                    catalog: self
                        .prefabs
                        .catalog(&self.project)
                        .ok_or("Working map catalog is unavailable")?,
                })
            })
            .transpose()?;
        capture.candidates.push(CapturedCandidate {
            saved: SavedCandidate {
                id,
                label: format!("Seed {} · {id}",self.project.seeds[0].value),
                recipe: self.project.clone(),
                zoom: self.zoom,
                pan: [self.pan.x, self.pan.y],
                missing: layout.solution.required_missing,
                monuments,
                routes: view.map_or_else(Vec::new, |view| mapper_studio::path_edit::capture(&self.project, &view.maps)),
                grid: self.grid,
                no_build: self.show_no_build,
                background: self.viewport_background().to_srgba_unmultiplied(),
                overview: 0,
                map: None,
                render: None,
                archive_issue: String::new(),
                image_issue: String::new(),
                palette: None,
                restrictions: None,
                font: None,
                font_ref: None,
            },
            overview: None,
            archive: None,
            palette,
            restrictions: self.restrictions.for_export(),
            font: self
                .monument_icons
                .labels
                .source(self.project.installation.as_ref()),
            working: Some(WorkingCapture {
                native,
                image: image.clone(),
                repaint_overview,
            }),
        });
        capture.workspace.open.push(id);
        if self.visible_generated_tab().is_none() {
            capture.workspace.active = Some(id);
        }
        Ok(capture)
    }
    pub(super) fn retain_generated_overview(
        &mut self,
        id: u64,
        ctx: &egui::Context,
    ) -> Result<Option<Arc<tempfile::TempPath>>, String> {
        let (image, _) = self
            .exportable_overview()
            .ok_or("Generation produced no accepted image")?;
        let image = Arc::clone(image);
        let key = OverviewKey { id, image: Arc::downgrade(&image) };
        if let Some((ready, file)) = &self.generation.overview_ready
            && ready.matches(&key)
        {
            return Ok(Some(file.clone()));
        }
        if self
            .generation
            .overview
            .as_ref()
            .is_some_and(|task| !task.key.matches(&key))
        {
            self.generation.overview = None;
        }
        if let Some(task) = &self.generation.overview {
            match task.receiver.try_recv() {
                Ok(result) => {
                    self.generation.overview = None;
                    let file = result?.ok_or("Map overview capture cancelled")?;
                    self.generation.overview_ready = Some((key, file.clone()));
                    return Ok(Some(file));
                }
                Err(TryRecvError::Disconnected) => return Err("Map overview worker stopped".into()),
                Err(TryRecvError::Empty) => return Ok(None),
            }
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let signal = cancel.clone();
        let (sender, receiver) = mpsc::channel();
        let repaint = ctx.clone();
        std::thread::spawn(move || {
            let result = mapper_studio::image_export::retain_overview(&image, || {
                signal.load(Ordering::Relaxed)
            }).map(|file| file.map(Arc::new));
            let _ = sender.send(result);
            repaint.request_repaint();
        });
        self.generation.overview = Some(OverviewTask {
            key,
            cancel,
            receiver,
        });
        Ok(None)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Font {
    metadata: serde_json::Value,
    sha256: String,
    alpha: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    grid: Option<SavedGrid>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedGrid {
    metadata: serde_json::Value,
    alpha: usize,
}
const SHARED_FONT_SCHEMA: u32 = 6;
const NAMED_RECIPES_SCHEMA: u32 = 7;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedCandidate {
    id: u64,
    label: String,
    recipe: Project,
    zoom: f32,
    pan: [f32; 2],
    missing: usize,
    monuments: Vec<map_monuments::Monument>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    routes: Vec<mapper_studio::path_edit::Route>,
    grid: bool,
    no_build: bool,
    background: [u8; 4],
    overview: usize,
    map: Option<usize>,
    render: Option<usize>,
    archive_issue: String,
    image_issue: String,
    palette: Option<mapper_studio::terrain_palette::Snapshot>,
    restrictions: Option<restriction_panel::Snapshot>,
    font: Option<Font>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    font_ref: Option<usize>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Workspace {
    schema: u32,
    results: Vec<SavedCandidate>,
    queue: Vec<QueueItem>,
    count: usize,
    randomize: [bool; 7],
    open: Vec<u64>,
    active: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    fonts: Vec<Font>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    edits: Vec<SavedEdit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recipes: Option<recipe_tabs::RecipeBook>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedEdit {
    map: u64,
    poses: Vec<(usize, [f32; 3])>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    move_limit_m: Option<u32>,
    #[serde(default, skip_serializing_if = "super::MonumentChanges::is_empty")]
    monuments: super::MonumentChanges,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    paths: Vec<mapper_studio::path_edit::Change>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_issue: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    issue: Option<EditIssue>,
}
#[derive(Default, Deserialize)]
#[serde(transparent)]
struct WorkspaceDocument(Option<Workspace>);

impl WorkspaceDocument {
    fn select_payloads(&self, payloads: usize) -> Result<Vec<bool>, String> {
        let mut selected = vec![false; payloads];
        let Some(saved) = &self.0 else { return Ok(selected) };
        if !(1..=NAMED_RECIPES_SCHEMA).contains(&saved.schema)
            || (!saved.edits.is_empty() && saved.schema < 3)
            || (!saved.fonts.is_empty() && saved.schema < SHARED_FONT_SCHEMA)
            || saved.fonts.len() > saved.results.len()
            || saved.edits.len() > saved.results.len()
            || saved.results.len() > MAX_BATCH + 1
            || saved.queue.len() > MAX_BATCH
            || !(1..=MAX_BATCH).contains(&saved.count)
            || saved.open.len() > MAX_BATCH + 1
        {
            return Err("Unsupported saved map workspace".into());
        }
        if saved.schema < NAMED_RECIPES_SCHEMA
            && (saved.recipes.is_some() || saved.queue.iter().any(|item| item.origin.is_some()))
        {
            return Err("Unsupported named recipes workspace".into());
        }
        if let Some(recipes) = &saved.recipes {
            recipes.validate()?;
        }
        if saved.queue.iter().filter_map(|item| item.origin.as_ref()).any(|origin| !origin.valid()) {
            return Err("Invalid queued recipe identity".into());
        }
        for map in &saved.results {
            if map.map.is_none() && map.render.is_some() {
                return Err("Saved render has no accepted map archive".into());
            }
            let font = match (map.font_ref, &map.font) {
                (Some(index), None) if saved.schema >= SHARED_FONT_SCHEMA => Some(
                    saved.fonts.get(index).ok_or("Saved map references a missing font")?,
                ),
                (Some(_), _) => return Err("Invalid saved map font reference".into()),
                (None, font) => font.as_ref(),
            };
            for index in [
                Some(map.overview), map.map, map.render, font.map(|font| font.alpha),
                font.and_then(|font| font.grid.as_ref()).map(|grid| grid.alpha),
            ].into_iter().flatten()
            {
                *selected.get_mut(index).ok_or("Saved map references a missing payload")? = true;
            }
        }
        Ok(selected)
    }
}
struct CapturedCandidate {
    saved: SavedCandidate,
    overview: Option<Arc<tempfile::TempPath>>,
    working: Option<WorkingCapture>,
    archive: Option<Arc<map_archive::Archive>>,
    palette: Option<Arc<mapper_studio::terrain_palette::Palette>>,
    restrictions: restriction_panel::Panel,
    font: Option<Arc<mapper_studio::map_labels::Prepared>>,
}
struct WorkingWorld {
    maps: Arc<papyrus_terrain::monument_composition::Composed>,
    solution: papyrus_terrain::placement::Solution,
    root: PathBuf,
    catalog: Arc<mapper_studio::prefab_assets::Catalog>,
}
struct WorkingCapture {
    native: Option<WorkingWorld>,
    image: Arc<egui::ColorImage>,
    repaint_overview: bool,
}

fn overview_style(
    palette: &mapper_studio::terrain_palette::Palette,
    colors: &mapper_studio::map_colors::Preferences,
    uses_base_image: bool,
) -> (mapper_studio::terrain_palette::Palette, bool) {
    let styled = palette.with_display_colors(colors);
    // The accepted colored image already represents the saved style.
    let repaint = uses_base_image && palette.cartography() != styled.cartography();
    (styled, repaint)
}
pub(crate) struct Capture {
    workspace: Workspace,
    candidates: Vec<CapturedCandidate>,
}
pub(crate) struct Restored {
    pub recipe: Project,
    pub recipes: recipe_tabs::RecipeBook,
    pub generation: Generation,
    pub open: Vec<u64>,
    pub active: Option<u64>,
}
fn insert(sources: &mut Vec<Source>, source: Source) -> usize {
    if let Some(index) = sources
        .iter()
        .position(|previous| previous.same_allocation(&source))
    {
        return index;
    }
    let index = sources.len();
    sources.push(source);
    index
}

fn restore_font<'a>(
    font: Font,
    installation: &mapper_studio::installation::Installation,
    blob: &impl Fn(usize) -> Result<&'a Arc<tempfile::TempPath>, String>,
) -> Result<Arc<mapper_studio::map_labels::Prepared>, String> {
    use sha2::Digest;
    let mut alpha = Vec::new();
    std::fs::File::open(blob(font.alpha)?.as_ref())
        .map_err(|e| e.to_string())?
        .take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut alpha)
        .map_err(|e| e.to_string())?;
    if alpha.len() > 32 * 1024 * 1024 || !valid_text(&font.sha256, 256) {
        return Err("Invalid saved map font".into());
    }
    let style = mapper_studio::map_labels::Style::decode(&font.metadata)?;
    let image = style.image(&alpha)?;
    let image_sha256 = format!(
        "{:x}",
        sha2::Sha256::digest(bytemuck::cast_slice(&image.pixels))
    );
    let grid = font.grid.map(|grid| {
        let mut alpha = Vec::new();
        std::fs::File::open(blob(grid.alpha)?.as_ref())
            .map_err(|e| e.to_string())?
            .take(4096 * 128 + 1)
            .read_to_end(&mut alpha)
            .map_err(|e| e.to_string())?;
        mapper_studio::map_grid::Appearance::from_snapshot(grid.metadata, alpha.into()).map(Arc::new)
    }).transpose()?;
    Ok(Arc::new(mapper_studio::map_labels::Prepared {
        metadata: Arc::new(font.metadata),
        installation: installation.clone(),
        sha256: font.sha256,
        style,
        image: Arc::new(image),
        image_sha256,
        alpha: alpha.into(),
        grid,
    }))
}

impl Capture {
    pub(crate) fn set_recipes(&mut self, recipes: &recipe_tabs::RecipeBook) -> Result<(), String> {
        recipes.validate()?;
        if !recipes.is_legacy_default() {
            self.workspace.recipes = Some(recipes.clone());
        }
        if self.workspace.recipes.is_some() || self.workspace.queue.iter().any(|item| item.origin.is_some()) {
            self.workspace.schema = NAMED_RECIPES_SCHEMA;
        }
        Ok(())
    }

    pub(crate) fn encode(
        mut self,
        cancel: &impl Fn() -> bool,
    ) -> Result<(Workspace, Vec<Source>), String> {
        let mut sources = Vec::new();
        let mut font_counts = HashMap::new();
        for font in self.candidates.iter().filter_map(|candidate| candidate.font.as_ref()) {
            *font_counts.entry(Arc::as_ptr(font)).or_insert(0usize) += 1;
        }
        let mut font_refs = HashMap::new();
        for mut candidate in self.candidates {
            if cancel() {
                return Err("Project save cancelled".into());
            }
            if let Some(working) = candidate.working {
                let mut image = working.image;
                if let Some(native) = working.native {
                    let input = mapper_studio::map_export::Input {
                        project: &candidate.saved.recipe,
                        maps: &native.maps,
                        solution: &native.solution,
                        root: &native.root,
                        catalog: &native.catalog,
                    };
                    match mapper_studio::map_export::retain(input, None, cancel) {
                        Ok(Some(file)) => {
                            let render = mapper_studio::image_snapshot::Snapshot::capture(
                                &native.maps,
                                cancel,
                            )?;
                            candidate.archive = Some(map_archive::Archive::new(
                                Arc::new(file), render, String::new(),
                            )?);
                        }
                        Ok(None) => return Err("Project save cancelled".into()),
                        Err(error) => candidate.saved.archive_issue = error,
                    }
                    if working.repaint_overview
                        && let Some(palette) = candidate.palette.as_deref()
                    {
                        image = Arc::new(mapper_studio::map_image::render_composed_viewport(
                            &native.maps,
                            Some(palette),
                            cancel,
                        )?
                        .ok_or("Project save cancelled")?
                        .image);
                    }
                }
                let file = mapper_studio::image_export::retain_overview(&image, cancel)?
                    .ok_or("Project save cancelled")?;
                candidate.overview = Some(Arc::new(file));
            }
            let mut saved = candidate.saved;
            saved.overview = insert(
                &mut sources,
                Source::File(candidate.overview.ok_or("Missing retained map overview")?),
            );
            if let Some(archive) = candidate.archive {
                saved.map = Some(insert(&mut sources, Source::Retained(archive.file.clone())));
                saved.render = archive
                    .image
                    .as_ref()
                    .map(|image| insert(&mut sources, Source::File(image.retained_file())));
                saved.image_issue = archive.image_issue.clone();
            }
            saved.palette = candidate.palette.as_ref().map(|palette| palette.snapshot());
            saved.restrictions = candidate.restrictions.snapshot(&saved.monuments);
            if let Some(font) = candidate.font {
                let identity = Arc::as_ptr(&font);
                if let Some(&index) = font_refs.get(&identity) {
                    saved.font_ref = Some(index);
                } else {
                    let record = Font {
                        metadata: font.metadata.as_ref().clone(),
                        sha256: font.sha256.clone(),
                        alpha: insert(&mut sources, Source::Bytes(font.alpha.clone())),
                        grid: font.grid.as_ref().map(|grid| {
                            let (metadata, alpha) = grid.snapshot();
                            SavedGrid { metadata, alpha: insert(&mut sources, Source::Bytes(alpha)) }
                        }),
                    };
                    if font_counts[&identity] > 1 {
                        let index = self.workspace.fonts.len();
                        self.workspace.fonts.push(record);
                        font_refs.insert(identity, index);
                        saved.font_ref = Some(index);
                        self.workspace.schema = self.workspace.schema.max(SHARED_FONT_SCHEMA);
                    } else {
                        saved.font = Some(record);
                    }
                }
            }
            self.workspace.results.push(saved);
        }
        Ok((self.workspace, sources))
    }
}
impl Generation {
    pub(crate) fn capture(&self, open: Vec<u64>, active: Option<u64>) -> Result<Capture, String> {
        let mut candidates = Vec::new();
        let mut edits = Vec::new();
        for result in &self.results {
            if let Some(edit) = self.edits.get(&result.id) {
                let poses: Vec<_> = edit.staged_poses().collect();
                let paths = edit.paths.changes().to_vec();
                if !poses.is_empty() || !paths.is_empty() || !edit.monuments.is_empty() {
                    // Keep the older queue-reference encoding while the card
                    // is still saved. Detached reports travel with the draft.
                    let last_issue = edit.last_issue.as_ref().filter(|issue| {
                        self.queue.iter().any(|item| {
                            item.id == issue.id && item.detail == issue.detail
                                && item.state == if issue.incomplete { State::Incomplete } else { State::Failed }
                        })
                    }).map(|issue| issue.id);
                    edits.push(SavedEdit {
                        map: result.id, move_limit_m: edit.move_limit_m, poses, paths, monuments: (*edit.monuments).clone(),
                        last_issue,
                        issue: if last_issue.is_none() { edit.last_issue.clone() } else { None },
                    });
                }
            }
            candidates.push(CapturedCandidate {
                saved: SavedCandidate {
                    id: result.id,
                    label: result.label.clone(),
                    recipe: result.recipe.clone(),
                    zoom: result.zoom,
                    pan: [result.pan.x, result.pan.y],
                    missing: result.missing,
                    monuments: result.monuments.clone(),
                    routes: result.routes.clone(),
                    grid: result.grid,
                    no_build: result.no_build,
                    background: result.background.to_srgba_unmultiplied(),
                    overview: 0,
                    map: None,
                    render: None,
                    archive_issue: result.archive_issue.clone(),
                    image_issue: String::new(),
                    palette: None,
                    restrictions: None,
                    font: None,
                    font_ref: None,
                },
                overview: Some(
                    result
                        .overview
                        .clone()
                        .ok_or("A generated map has no retained overview")?,
                ),
                working: None,
                archive: result.archive.clone(),
                palette: result.palette.clone(),
                restrictions: result.restrictions.for_export(),
                font: result.map_font.clone(),
            });
        }
        Ok(Capture {
            workspace: Workspace {
                schema: if self.queue.iter().any(|item| item.origin.is_some()) { NAMED_RECIPES_SCHEMA }
                    else if edits.is_empty() { 2 }
                    else if edits.iter().any(|edit| !edit.monuments.is_empty()) { 5 }
                    else if edits.iter().any(|edit| edit.issue.is_some()) { 4 } else { 3 },
                results: Vec::new(),
                queue: self.queue.clone(),
                count: self.count,
                randomize: self.randomize,
                open,
                active,
                fonts: Vec::new(),
                edits,
                recipes: None,
            },
            candidates,
        })
    }
    pub(crate) fn restore(
        path: &std::path::Path,
        ctx: &egui::Context,
        cancel: &(impl Fn() -> bool + Sync),
    ) -> Result<Option<Restored>, String> {
        let Some(loaded) = load_workspace_selected(
            path, WorkspaceDocument::select_payloads, cancel,
        )? else {
            return Ok(None);
        };
        let recipe = loaded.manifest.recipe;
        let Some(saved) = loaded.manifest.workspace.0 else {
            return Ok(Some(Restored {
                recipe,
                recipes: Default::default(),
                generation: Self::default(),
                open: Vec::new(),
                active: None,
            }));
        };
        let retained = |index: usize| loaded.blobs.get(index).and_then(Option::as_ref);
        let blob = |index: usize| {
            retained(index)
                .ok_or_else(|| "Saved map references a missing payload".to_string())
        };
        let mut generation = Self {
            count: saved.count,
            randomize: saved.randomize,
            ..Default::default()
        };
        let mut ids = std::collections::BTreeSet::new();
        let mut fonts = HashMap::new();
        let mut shared_fonts: Vec<_> = saved.fonts.into_iter().map(Some).collect();
        let mut decoded_fonts = HashMap::<usize, Arc<mapper_studio::map_labels::Prepared>>::new();
        let mut map_headers = HashSet::new();
        let mut render_snapshots = HashMap::new();
        // Older workspaces can omit the active tab. Resolve the same matching
        // recipe the desktop will open before deciding which PNG to decode.
        let active = saved.active.or_else(|| {
            saved.results.iter().rev().find(|map| map.recipe == recipe).map(|map| map.id)
        });
        // Bundle loading checks every span and shares the allocation of verified
        // duplicates. Use that identity even when a file has separate receipts.
        let active_overview = saved
            .results
            .iter()
            .find(|map| Some(map.id) == active)
            .and_then(|map| retained(map.overview));
        let mut unique_overviews = HashSet::new();
        let mut inactive_overviews = Vec::new();
        for saved in saved.results {
            if cancel() {
                return Ok(None);
            }
            saved.recipe.validate()?;
            if saved.id == 0
                || saved.id == u64::MAX
                || !ids.insert(saved.id)
                || !valid_text(&saved.label, 256)
                || !saved.zoom.is_finite()
                || !(0.1..=16.).contains(&saved.zoom)
                || saved.pan.iter().any(|n| !n.is_finite() || n.abs() > 1e8)
                || saved.monuments.len() > 65536
                || saved.missing > saved.recipe.targets.len()
                || saved.archive_issue.len() > 1024 * 1024
                || saved.image_issue.len() > 1024 * 1024
                || saved.monuments.iter().any(|m| {
                    !valid_text(&m.name, 1024)
                        || m.prefab.as_ref().is_some_and(|p| {
                            p.len() > 512 || !p.starts_with("assets/") || !p.ends_with(".prefab")
                        })
                        || m.position
                            .iter()
                            .chain(m.restriction_origin.iter().flatten())
                            .any(|n| !n.is_finite() || n.abs() > 100000.)
                        || !m.yaw.is_finite()
                })
            {
                return Err("Invalid saved map properties".into());
            }
            let overview = blob(saved.overview)?;
            if !active_overview.is_some_and(|active| Arc::ptr_eq(active, overview))
                && unique_overviews.insert(Arc::as_ptr(overview))
            {
                inactive_overviews.push(Arc::clone(overview));
            }
            let archive = match saved.map {
                Some(index) => {
                    let file = blob(index)?;
                    if map_headers.insert(Arc::as_ptr(file)) {
                        let mut header = [0; 12];
                        std::fs::File::open(file.as_ref())
                            .map_err(|e| e.to_string())?
                            .read_exact(&mut header)
                            .map_err(|e| e.to_string())?;
                        if header[..4] != 10u32.to_le_bytes()
                            || u64::from_le_bytes(header[4..].try_into().unwrap()) > i64::MAX as u64
                        {
                            return Err("Unsupported saved game map archive".into());
                        }
                    }
                    let image = if let Some(index) = saved.render {
                        let render = blob(index)?;
                        let key = (Arc::as_ptr(render), saved.recipe.size);
                        let snapshot = match render_snapshots.entry(key) {
                            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                            std::collections::hash_map::Entry::Vacant(entry) => entry.insert(
                                mapper_studio::image_snapshot::Snapshot::from_retained_file(
                                    render.clone(),
                                    saved.recipe.size,
                                )?,
                            ),
                        };
                        Some(snapshot.clone())
                    } else {
                        None
                    };
                    Some(map_archive::Archive::new(file.clone(), image, saved.image_issue)?)
                }
                None => None,
            };
            let palette = saved
                .palette
                .map(|snapshot| {
                    let installation = saved.recipe.installation.as_ref()
                        .ok_or("Saved map palette requires its installation identity")?;
                    let previous = generation.results.iter()
                        .filter(|result| result.recipe.installation.as_ref() == Some(installation))
                        .filter_map(|result| result.palette.as_ref())
                        .find(|palette| palette.matches_snapshot(&snapshot));
                    if let Some(previous) = previous {
                        Ok(previous.clone())
                    } else {
                        mapper_studio::terrain_palette::Palette::from_snapshot(snapshot, installation)
                            .map(Arc::new)
                    }
                })
                .transpose()?;
            let font_installation = || saved.recipe.installation.as_ref()
                .ok_or("Saved map font requires its installation identity");
            let font = match (saved.font_ref, saved.font) {
                (Some(index), _) => {
                    let installation = font_installation()?;
                    if let Some(font) = decoded_fonts.get(&index) {
                        if &font.installation != installation {
                            return Err("Saved maps share a font across different Rust installations".into());
                        }
                        Some(Arc::clone(font))
                    } else {
                        let record = shared_fonts.get_mut(index).and_then(Option::take)
                            .ok_or("Saved map references a missing font")?;
                        let font = restore_font(record, installation, &blob)?;
                        decoded_fonts.insert(index, font.clone());
                        Some(font)
                    }
                }
                (None, Some(font)) => {
                    let alpha_file = blob(font.alpha)?;
                    let font_key = (
                        Arc::as_ptr(alpha_file),
                        font.sha256.clone(),
                        serde_json::to_vec(&(&font.metadata, &saved.recipe.installation, &font.grid))
                            .map_err(|e| e.to_string())?,
                    );
                    if let Some(font) = fonts.get(&font_key) {
                        Some(Arc::clone(font))
                    } else {
                        let font = restore_font(font, font_installation()?, &blob)?;
                        fonts.insert(font_key, font.clone());
                        Some(font)
                    }
                }
                (None, None) => None,
            };
            mapper_studio::path_edit::validate_routes(&saved.routes, saved.recipe.size)?;
            let restrictions = restriction_panel::Panel::from_snapshot(
                saved.restrictions, &saved.recipe,
            )?;
            generation.results.push(Candidate {
                identity: None,
                overview: Some(overview.clone()),
                archive,
                archive_issue: saved.archive_issue,
                id: saved.id,
                label: saved.label,
                recipe: saved.recipe,
                image: None,
                zoom: saved.zoom,
                pan: Vec2::from(saved.pan),
                placed: saved.monuments.iter().filter(|marker| !marker.is_scenery_marker()).count(),
                missing: saved.missing,
                restrictions,
                monuments: saved.monuments,
                routes: saved.routes,
                grid: saved.grid,
                no_build: saved.no_build,
                map_font: font,
                background: Color32::from_rgba_unmultiplied(
                    saved.background[0],
                    saved.background[1],
                    saved.background[2],
                    saved.background[3],
                ),
                palette,
            });
        }
        for saved_edit in saved.edits {
            let result = generation.results.iter_mut().find(|result| result.id == saved_edit.map)
                .ok_or("A saved map draft refers to a missing map")?;
            if (saved_edit.poses.is_empty() && saved_edit.paths.is_empty() && saved_edit.monuments.is_empty())
                || (saved.schema < 5 && !saved_edit.monuments.is_empty())
                || saved_edit.move_limit_m.is_some_and(|limit| !(1..=result.recipe.size).contains(&limit))
                || saved_edit.poses.len() > papyrus_terrain::placement::MAX_TARGETS
                || generation.edits.contains_key(&saved_edit.map)
            {
                return Err("Invalid saved map draft".into());
            }
            if let Some(issue) = &saved_edit.issue {
                if saved.schema < 4 || saved_edit.last_issue.is_some()
                    || issue.id == 0 || issue.id == u64::MAX || issue.detail.len() > 1024 * 1024
                {
                    return Err("Invalid saved rebuild issue".into());
                }
                generation.next_id = generation.next_id.max(issue.id + 1);
            }
            let last_issue = saved_edit.issue.or_else(|| {
                let id = saved_edit.last_issue?;
                saved.queue.iter().find(|item| item.id == id).and_then(EditIssue::from_item)
            });
            saved_edit.monuments.apply(&mut result.recipe.clone(), &result.monuments)?;
            let mut edit = MapEdit {
                last_issue, move_limit_m: saved_edit.move_limit_m, monuments: Arc::new(saved_edit.monuments), ..Default::default()
            };
            let half = result.recipe.size as f32 / 2.;
            for (index, pose @ [x, z, yaw]) in saved_edit.poses {
                if !result.monuments.get(index).is_some_and(|monument| movable_monument(&result.recipe, monument))
                    || !(-half..=half).contains(&x)
                    || !(-half..=half).contains(&z)
                    || !(0. ..=360.).contains(&yaw)
                    || edit.poses.insert(index, pose).is_some()
                {
                    return Err("Invalid saved monument move".into());
                }
            }
            edit.paths = path_editor::Draft::restore(&result.routes, saved_edit.paths, result.recipe.size)?;
            generation.edits.insert(saved_edit.map, edit);
        }
        let mut queued = std::collections::BTreeSet::new();
        for mut item in saved.queue {
            item.recipe.validate()?;
            if item.id == 0
                || item.id == u64::MAX
                || !queued.insert(item.id)
                || !valid_text(&item.label, 256)
                || item.detail.len() > 1024 * 1024
            {
                return Err("Invalid saved queue item".into());
            }
            let result = generation
                .results
                .iter()
                .find(|result| result.id == item.id);
            if (item.state.retains_result()
                && result.is_none_or(|result| result.recipe != item.recipe))
                || (!item.state.retains_result() && result.is_some())
            {
                return Err("Saved queue state differs from its generated maps".into());
            }
            if item.state == State::Ready
                && let Some(result) = result
                && let Some(issue) = completion_issue(&result.recipe, result.missing, result.archive.is_some(), &result.archive_issue)
            {
                item.state = State::Incomplete;
                item.detail = issue;
            }
            if matches!(item.state, State::Running | State::Waiting) {
                item.state = State::Cancelled;
                item.detail = "Generation was unfinished when this project was saved".into();
            }
            generation.next_id = generation.next_id.max(item.id + 1);
            generation.queue.push(item);
        }
        if saved.open.iter().any(|id| !ids.contains(id))
            || saved
                .open
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != saved.open.len()
            || saved.active.is_some_and(|id| !saved.open.contains(&id))
        {
            return Err("Saved viewport tabs reference an unknown map".into());
        }
        if let Some(id) = ids.last() {
            generation.next_id = generation.next_id.max(id + 1);
        }
        let Some(image) = load_overviews(
            &inactive_overviews,
            active_overview.map(|file| file.as_ref().as_ref()),
            cancel,
        )? else {
            return Ok(None);
        };
        if let (Some(id), Some(image)) = (active, image) {
            let map = generation.results.iter_mut().find(|map| map.id == id)
                .expect("active map was resolved from the validated workspace");
            map.image = Some(Ok(ctx.load_texture(
                format!("reopened-map-{id}"),
                image,
                egui::TextureOptions::LINEAR,
            )));
            generation.remember_preview(id);
        }
        Ok(Some(Restored {
            recipe,
            recipes: saved.recipes.unwrap_or_default(),
            generation,
            open: saved.open,
            active,
        }))
    }
}
fn valid_text(text: &str, limit: usize) -> bool {
    !text.is_empty() && text.len() <= limit && !text.chars().any(char::is_control)
}
fn open_overview(
    path: &std::path::Path,
) -> Result<(png::Reader<std::io::BufReader<std::fs::File>>, usize), String> {
    let mut decoder = png::Decoder::new(std::io::BufReader::new(
        std::fs::File::open(path).map_err(|e| e.to_string())?,
    ));
    decoder.set_limits(png::Limits {
        bytes: 32 * 1024 * 1024,
    });
    let reader = decoder.read_info().map_err(|e| e.to_string())?;
    let info = reader.info();
    if info.width == 0
        || info.height == 0
        || info.width > 2048
        || info.height > 2048
        || info.color_type != png::ColorType::Rgba
        || info.bit_depth != png::BitDepth::Eight
    {
        return Err("Invalid saved map overview".into());
    }
    let rgba_bytes = reader
        .output_buffer_size()
        .filter(|&bytes| bytes <= 16 * 1024 * 1024)
        .ok_or("Saved overview exceeds its image limit")?;
    Ok((reader, rgba_bytes))
}

fn validate_overview(path: &std::path::Path, cancel: &impl Fn() -> bool) -> Result<bool, String> {
    if cancel() {
        return Ok(false);
    }
    let (mut reader, _) = open_overview(path)?;
    // next_row flushes the frame's remaining compressed data on its final call,
    // just as next_frame does. The decoder reuses a single output row.
    loop {
        if cancel() {
            return Ok(false);
        }
        if reader.next_row().map_err(|e| e.to_string())?.is_none() {
            return Ok(true);
        }
    }
}

/// Returns no result on cancellation. A completed project can have no active image.
fn load_overviews(
    overviews: &[Arc<tempfile::TempPath>],
    active: Option<&std::path::Path>,
    cancel: &(impl Fn() -> bool + Sync),
) -> Result<Option<Option<egui::ColorImage>>, String> {
    use mapper_studio::generation_resources::{current, try_with_cpu_pool};

    if cancel() {
        return Ok(None);
    }
    let read_active = || match active {
        Some(path) => read_overview(path, cancel),
        None => Ok(None),
    };
    let complete = |image: Option<egui::ColorImage>| {
        (!cancel() && (active.is_none() || image.is_some())).then_some(image)
    };
    if overviews.len() + usize::from(active.is_some()) > 1 && current().effective_workers() > 1
        && let Some(complete) = try_with_cpu_pool(|pool| {
            let workers = pool.current_num_threads().min(4);
            let validate_batch = |batch: &[Arc<tempfile::TempPath>]| {
                batch.par_iter().map(|path| validate_overview(path, cancel)).collect::<Vec<_>>()
            };
            let validate_ordered = |checks: Vec<Result<bool, String>>| -> Result<bool, String> {
                for check in checks {
                    if !check? {
                        return Ok(false);
                    }
                }
                Ok(true)
            };
            let (image, remaining) = if active.is_some() {
                let (first, remaining) = overviews.split_at(overviews.len().min(workers - 1));
                let (image, checks) = pool.install(|| {
                    rayon::join(read_active, || validate_batch(first))
                });
                if !validate_ordered(checks)? {
                    return Ok(None);
                }
                (image, remaining)
            } else {
                (Ok(None), overviews)
            };
            // Keep hidden-overview error order even if the active decode finishes first.
            for batch in remaining.chunks(workers) {
                if !validate_ordered(pool.install(|| validate_batch(batch)))? {
                    return Ok(None);
                }
            }
            Ok(complete(image?))
        })?
    {
        return Ok(complete);
    }
    for path in overviews {
        if !validate_overview(path, cancel)? {
            return Ok(None);
        }
    }
    Ok(complete(read_active()?))
}

pub(super) fn read_overview(
    path: &std::path::Path,
    cancel: &impl Fn() -> bool,
) -> Result<Option<egui::ColorImage>, String> {
    if cancel() {
        return Ok(None);
    }
    let (mut reader, rgba_bytes) = open_overview(path)?;
    let info = reader.info();
    if info.interlaced || info.animation_control.is_some() {
        let mut rgba = vec![0; rgba_bytes];
        let image = reader.next_frame(&mut rgba).map_err(|e| e.to_string())?;
        return Ok((!cancel()).then(|| {
            egui::ColorImage::from_rgba_unmultiplied(
                [image.width as usize, image.height as usize],
                &rgba[..image.buffer_size()],
            )
        }));
    }

    let size = [info.width as usize, info.height as usize];
    let mut pixels = Vec::with_capacity(size[0] * size[1]);
    loop {
        if cancel() {
            return Ok(None);
        }
        let Some(row) = reader.next_row().map_err(|e| e.to_string())? else {
            break;
        };
        let (rgba, tail) = row.data().as_chunks::<4>();
        if !tail.is_empty() || rgba.len() != size[0] || pixels.len() == size[0] * size[1] {
            return Err("Invalid saved map overview rows".into());
        }
        pixels.extend(
            rgba.iter()
                .map(|&[r, g, b, a]| Color32::from_rgba_unmultiplied(r, g, b, a)),
        );
    }
    if pixels.len() != size[0] * size[1] {
        return Err("Incomplete saved map overview".into());
    }
    Ok((!cancel()).then(|| egui::ColorImage::new(size, pixels)))
}

#[cfg(test)]
mod tests;
