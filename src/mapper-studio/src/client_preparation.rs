//! Prepare supported generation data once when an installation is selected.
use crate::{Project, prefab_assets::Catalog, prefab_textures::Bindings};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

mod rebind;
pub use rebind::{ReboundRecipe, rebind_recipe};

pub struct Step {
    pub name: &'static str,
    pub elapsed: Duration,
    pub error: Option<String>,
}
#[derive(Default)]
pub struct Report {
    pub steps: Vec<Step>,
    pub palette: Option<std::sync::Arc<crate::terrain_palette::Palette>>,
    pub restrictions: Option<Catalog>,
    pub tunnel_markers: Option<TunnelMarkerOffsets>,
    pub monuments: Option<crate::monument_source::Prepared>,
    pub roads: Option<ReadyRoads>,
    pub decoration: Option<std::sync::Arc<crate::decor_source::Source>>,
    pub uniform_cliffs: Option<ReadyCliffs>,
    pub rails: Option<ReadyRails>,
    pub rivers: Option<crate::river_generation::Recipe>,
    pub river_objects: Option<crate::road_object_source::Source>,
    pub map_labels: Option<std::sync::Arc<crate::map_labels::Prepared>>,
    pub identities: Option<std::sync::Arc<ReadyIdentities>>,
    pub default_objects: Option<ReadyObjects>,
    road_object_sha256: Option<String>,
    road_objects: Option<crate::road_object_source::Source>,
}
pub struct ReadyIdentities {
    pub(crate) source: std::sync::Arc<crate::prefab_identities::Source>,
    pub addons: crate::monument_addons::Dependencies,
}
impl ReadyIdentities {
    pub fn check_monument_content(&self, project: &Project) -> Result<(), crate::monument_addons::OverlayIssue> {
        self.addons.check_recipe(project)?;
        let _ = crate::custom_monuments::assemble(project, 0, &self.source.prefabs, || false)?;
        Ok(())
    }

    pub fn prefab_paths(&self) -> impl Iterator<Item = &str> {
        self.source.prefabs.keys().map(String::as_str)
    }

    pub fn prefab_id(&self, path: &str) -> Option<u32> {
        self.source.prefabs.get(path).copied()
    }

    pub fn prefab_path(&self, id: u32) -> Option<&str> {
        self.source.prefabs.iter().find_map(|(path, &candidate)| {
            (candidate == id).then_some(path.as_str())
        })
    }
}
pub struct TunnelMarkerOffsets {
    pub catalog_sha256: String,
    pub offsets: BTreeMap<String, [f32; 2]>,
}
pub struct ReadyRoads {
    pub source: crate::infrastructure::Source,
    pub previous_source: Option<crate::infrastructure::Source>,
    pub surface: String,
    pub roadside: String,
    pub joined: Option<crate::road_cost_source::Profile>,
    pub ordered: crate::source_road_planning::Policy,
}
pub struct ReadyCliffs {
    root: PathBuf,
    catalog: String,
    road_source: String,
    source: String,
    coastal: Option<String>,
    vines: Option<String>,
}
impl ReadyCliffs {
    pub fn apply(&self, project: &mut Project, root: &Path, catalog: &str) -> bool {
        if self.root != root || self.catalog != catalog {
            return false;
        }
        let Some(objects) = project.objects.as_mut() else {
            return false;
        };
        if objects.engine != crate::object_generation::VERSION
            || objects.road_source_sha256 != self.road_source
        {
            return false;
        }
        let mut changed = false;
        if objects.uniform_cliffs_source_sha256.is_none() {
            objects.uniform_cliffs_source_sha256 = Some(self.source.clone());
            changed = true;
        }
        if objects.coastal_cliffs_source_sha256.is_none() && self.coastal.is_some() {
            objects.coastal_cliffs_source_sha256.clone_from(&self.coastal);
            changed = true;
        }
        if objects.jungle_vines_source_sha256.is_none() && self.vines.is_some() {
            objects.jungle_vines_source_sha256.clone_from(&self.vines);
            changed = true;
        }
        changed
    }
}
/// Shared scenery binding from the same prepared sources used for map generation.
pub struct ReadyObjects {
    root: PathBuf,
    catalog: String,
    infrastructure: String,
    decoration: String,
    monuments: String,
    recipe: crate::object_generation::Recipe,
}
impl ReadyObjects {
    pub fn recipe(&self, project: &Project, root: &Path, catalog: &str) -> Option<crate::object_generation::Recipe> {
        (self.root == root && self.catalog == catalog
            && project.roads.as_ref().is_some_and(|r| r.source_sha256 == self.infrastructure)
            && project.decoration.as_ref().is_some_and(|r| r.source_sha256 == self.decoration)
            && project.monument_rules.as_ref().is_some_and(|r| r.source_sha256 == self.monuments))
            .then(|| self.recipe.clone())
    }
}

pub struct ReadyRails {
    root: PathBuf,
    catalog: String,
    bindings: String,
    prepared: crate::rail_extract::Prepared,
}
pub fn load(
    root: &Path,
    source: &crate::installation::Installation,
    project: &Project,
    cancelled: impl Fn() -> bool,
) -> Result<(Catalog, Bindings, Report), String> {
    load_with_overlays(root, source, project, true, cancelled)
}

pub fn load_for_editor(
    root: &Path,
    source: &crate::installation::Installation,
    project: &Project,
    cancelled: impl Fn() -> bool,
) -> Result<(Catalog, Bindings, Report), String> {
    load_with_overlays(root, source, project, false, cancelled)
}

fn load_with_overlays(
    root: &Path,
    source: &crate::installation::Installation,
    project: &Project,
    include_restrictions: bool,
    cancelled: impl Fn() -> bool,
) -> Result<(Catalog, Bindings, Report), String> {
    let _scripts = crate::metadata_runtime::script_workspace();
    let _verification = crate::installation::VerificationScope::default();
    let started = Instant::now();
    #[cfg(windows)]
    verify_core_sources(
        root,
        crate::generation_resources::current().effective_workers(),
        &cancelled,
    )?;
    let verification_elapsed = started.elapsed();
    let started = Instant::now();
    let (catalog, bindings) = {
        let _progress = crate::loading_progress::begin("Loading map assets", None);
        crate::asset_discovery::prepare(
            root,
            source,
            project.monument_assets.as_ref(),
            &cancelled,
        )?
    };
    let assets_elapsed = started.elapsed();
    let mut report =
        prepare_with_overlays(root, project, &catalog, &bindings, include_restrictions, &cancelled)?;
    report.steps.splice(
        0..0,
        [
            Step {
                name: "Checking Rust files",
                elapsed: verification_elapsed,
                error: None,
            },
            Step {
                name: "Loading map assets",
                elapsed: assets_elapsed,
                error: None,
            },
        ],
    );
    Ok((catalog, bindings, report))
}

#[cfg(windows)]
const CORE_SOURCES: [&str; 11] = [
    "Bundles/shared/assetscenes.bundle",
    "Bundles/shared/content.bundle",
    "Bundles/shared/monuments.bundle",
    "Bundles/maps/maps.bundle",
    "GameAssembly.dll",
    "RustClient_Data/globalgamemanagers",
    "RustDedicated_Data/globalgamemanagers",
    "RustClient_Data/il2cpp_data/Metadata/global-metadata.dat",
    "RustDedicated_Data/il2cpp_data/Metadata/global-metadata.dat",
    "RustClient_Data/Managed/Assembly-CSharp.dll",
    "RustDedicated_Data/Managed/Assembly-CSharp.dll",
];

#[cfg(windows)]
const SOURCE_LIMIT: u64 = 8 * 1024 * 1024 * 1024;

#[cfg(windows)]
struct PendingSource {
    relative: &'static str,
    path: PathBuf,
    file: std::fs::File,
    bytes: u64,
}

#[cfg(windows)]
fn verify_core_sources(
    root: &Path,
    workers: usize,
    cancelled: &impl Fn() -> bool,
) -> Result<(), String> {
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

    let progress = crate::loading_progress::begin("Verifying Rust files", None);
    let reporter = progress.reporter();
    let mut pending = Vec::new();
    for relative in CORE_SOURCES {
        if cancelled() {
            return Err("Client preparation cancelled".into());
        }
        let path = root.join(relative);
        let file = match crate::source_lease::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("Cannot read {relative}: {error}")),
        };
        if let Err(file) = crate::installation::reuse_open_hash(&path, relative, file, SOURCE_LIMIT) {
            let bytes = file.metadata().map_err(|error| error.to_string())?.len();
            pending.push(PendingSource { relative, path, file, bytes });
        }
    }
    if pending.is_empty() {
        return Ok(());
    }
    let _receipts = crate::source_lease::batch_persistent_writes();
    if workers < 2 || pending.len() < 2 {
        return hash_core_sources(pending, cancelled);
    }

    // Keep the two longest reads on separate workers and balance the remainder
    // by file size. The original read-only handles travel with the jobs.
    pending.sort_unstable_by_key(|file| std::cmp::Reverse(file.bytes));
    let [mut left, mut right] = [Vec::new(), Vec::new()];
    let [mut left_bytes, mut right_bytes] = [0_u64; 2];
    for file in pending {
        if left_bytes <= right_bytes {
            left_bytes = left_bytes.saturating_add(file.bytes);
            left.push(file);
        } else {
            right_bytes = right_bytes.saturating_add(file.bytes);
            right.push(file);
        }
    }
    let user_signal = reporter.cancel_token().unwrap_or_default();
    let signal = Arc::new(AtomicBool::new(false));
    let run = |files| {
        let _verification = crate::installation::VerificationScope::default();
        let _progress =
            crate::loading_progress::begin("Verifying Rust files", Some(user_signal.clone()));
        hash_core_sources(files, &|| signal.load(Ordering::Relaxed))?;
        // Copy the verified handles before the worker scope drops. The caller
        // imports them into its still-live preparation scope.
        crate::source_lease::fork()
    };
    let (left, right) = run_preparation_workers(
        &|| cancelled() || user_signal.load(Ordering::Relaxed),
        signal.clone(),
        &reporter,
        || run(left),
        || run(right),
    )?;
    let _left = left.enter();
    let _right = right.enter();
    Ok(())
}

#[cfg(windows)]
fn hash_core_sources(
    files: Vec<PendingSource>,
    cancelled: &impl Fn() -> bool,
) -> Result<(), String> {
    for file in files {
        let result = crate::installation::hash_open_file(
            file.path, file.relative, file.file, SOURCE_LIMIT, cancelled,
        );
        result.map_err(|error| {
            if error == "Installation inspection cancelled" {
                "Client preparation cancelled".into()
            } else {
                error
            }
        })?;
    }
    Ok(())
}
impl ReadyRails {
    pub fn matches(&self, project: &Project, catalog: &Catalog, bindings: &Bindings) -> bool {
        self.root == bindings.installation_root()
            && self.catalog == catalog.sha256
            && self.bindings == bindings.sha256
            && project.installation.as_ref() == Some(&catalog.installation)
            && project
                .roads
                .as_ref()
                .is_some_and(|r| r.source_sha256 == self.prepared.source.infrastructure_digest())
            && project.rails.as_ref().is_none_or(|r| {
                r.source_sha256 == self.prepared.source.sha256
                    && r.mesh_sha256 == self.prepared.meshes.sha256
            })
    }
    pub fn bind(
        &self,
        project: &Project,
        catalog: &Catalog,
        bindings: &Bindings,
    ) -> Result<crate::rail_generation::Recipe, String> {
        if !self.matches(project, catalog, bindings) {
            return Err(
                "Prepared rails belong to a different installation or asset version".into(),
            );
        }
        let settings = project
            .rails
            .as_ref()
            .map(|r| r.settings.clone())
            .unwrap_or_else(|| crate::rail_generation::initial_settings(project.master_seed));
        crate::rail_generation::bind_prepared(project, catalog, bindings, &self.prepared, settings)
    }
}
fn step(
    report: &mut Report,
    name: &'static str,
    cancelled: &impl Fn() -> bool,
    work: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    if cancelled() {
        return Err("Client preparation cancelled".into());
    }
    let progress = crate::loading_progress::begin(name, None);
    progress.reporter().stage(name);
    let start = Instant::now();
    let result = work();
    if cancelled() {
        return Err("Client preparation cancelled".into());
    }
    report.steps.push(Step {
        name,
        elapsed: start.elapsed(),
        error: result.err(),
    });
    Ok(())
}
pub fn prepare(
    root: &Path,
    project: &Project,
    catalog: &Catalog,
    bindings: &Bindings,
    cancelled: impl Fn() -> bool,
) -> Result<Report, String> {
    prepare_with_overlays(root, project, catalog, bindings, true, cancelled)
}

fn prepare_with_overlays(
    root: &Path,
    project: &Project,
    catalog: &Catalog,
    bindings: &Bindings,
    include_restrictions: bool,
    cancelled: impl Fn() -> bool,
) -> Result<Report, String> {
    let _scripts = crate::metadata_runtime::script_workspace();
    let _verification = crate::installation::VerificationScope::default();
    #[cfg(windows)]
    if let workers @ 2..=3 = preparation_worker_count() {
        use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
        let progress = crate::loading_progress::begin("Preparing generation data", None);
        let reporter = progress.reporter();
        let user_signal = reporter.cancel_token().unwrap_or_default();
        let signal = Arc::new(AtomicBool::new(false));
        let separate_scene_data = workers == 3;
        // Duplicate existing deny-write handles. Each worker keeps the exact
        // verified receipts alive without hashing shared bundles again.
        let mut jobs = vec![
            preparation_job("Monument data", &signal, &user_signal, move |cancelled| {
                prepare_monuments(root, project, catalog, bindings, include_restrictions, separate_scene_data, cancelled)
            })?,
            preparation_job("Paths and scenery", &signal, &user_signal, move |cancelled| {
                prepare_paths(root, project, catalog, bindings, separate_scene_data, cancelled)
            })?,
        ];
        if separate_scene_data {
            jobs.push(preparation_job("Scene data", &signal, &user_signal, move |cancelled| {
                prepare_scene_data(root, project, catalog, include_restrictions, cancelled)
            })?);
        }
        let mut reports = run_preparation_jobs(
            &|| cancelled() || user_signal.load(Ordering::Relaxed), signal,
            &reporter,
            jobs,
        )?.into_iter();
        let mut report = combine_reports(reports.next().unwrap(), reports.next().unwrap());
        if let Some(scene) = reports.next() {
            merge_scene_report(&mut report, scene);
        }
        prepare_default_objects(&mut report, root, catalog, &cancelled)?;
        return Ok(report);
    }
    let monuments = prepare_monuments(root, project, catalog, bindings, include_restrictions, false, &cancelled)?;
    let paths = prepare_paths(root, project, catalog, bindings, false, &cancelled)?;
    let mut report = combine_reports(monuments, paths);
    prepare_default_objects(&mut report, root, catalog, &cancelled)?;
    Ok(report)
}
fn combine_reports(mut monuments: Report, paths: Report) -> Report {
    monuments.steps.extend(paths.steps);
    monuments.roads = paths.roads;
    monuments.rails = paths.rails;
    monuments.rivers = paths.rivers;
    monuments.river_objects = paths.river_objects;
    monuments.decoration = paths.decoration;
    monuments.uniform_cliffs = paths.uniform_cliffs;
    monuments.identities = paths.identities;
    monuments.road_object_sha256 = paths.road_object_sha256;
    monuments.road_objects = paths.road_objects;
    monuments
}

fn prepare_default_objects(
    report: &mut Report, root: &Path, catalog: &Catalog, cancelled: &impl Fn() -> bool,
) -> Result<(), String> {
    let (Some(road), Some(monuments), Some(decoration), Some(cliffs)) = (
        report.road_objects.take(), report.monuments.as_ref(),
        report.decoration.as_ref(), report.uniform_cliffs.as_ref(),
    ) else { return Ok(()) };
    let (Some(coastal), Some(vines)) = (&cliffs.coastal, &cliffs.vines) else { return Ok(()) };
    let monuments = monuments.source().clone();
    let decoration = decoration.clone();
    let mut recipe = crate::object_generation::Recipe::new(road.sha256.clone());
    recipe.uniform_cliffs_source_sha256 = Some(cliffs.source.clone());
    recipe.coastal_cliffs_source_sha256 = Some(coastal.clone());
    recipe.jungle_vines_source_sha256 = Some(vines.clone());
    let mut ready = None;
    step(report, "Binding shared scenery placement", cancelled, || {
        recipe.validate()?;
        crate::object_generation::verify_prepared_sources(catalog, &road, &decoration, &monuments)?;
        ready = Some(ReadyObjects {
            root: root.to_owned(),
            catalog: catalog.sha256.clone(),
            infrastructure: road.infrastructure_sha256,
            decoration: decoration.sha256.clone(),
            monuments: monuments.sha256.clone(),
            recipe,
        });
        Ok(())
    })?;
    report.default_objects = ready;
    Ok(())
}

#[cfg(windows)]
fn merge_scene_report(report: &mut Report, scene: Report) {
    report.restrictions = scene.restrictions;
    report.decoration = scene.decoration;
    // Preserve the public preparation report's order across worker counts.
    for step in scene.steps {
        let previous = match step.name {
            "Preparing no-build zones" => "Preparing map text",
            "Preparing monument volumes" => "Preparing offshore monuments",
            "Preparing rocks, cliffs and scenery" => "Preparing river objects",
            "Preparing cable connections" => "Preparing substations and powerlines",
            _ => "",
        };
        let index = report.steps.iter().position(|step| step.name == previous)
            .map_or(report.steps.len(), |index| index + 1);
        report.steps.insert(index, step);
    }
}

#[cfg(windows)]
fn prepare_scene_data(
    root: &Path,
    project: &Project,
    catalog: &Catalog,
    include_restrictions: bool,
    cancelled: impl Fn() -> bool,
) -> Result<Report, String> {
    let mut report = Report::default();
    if include_restrictions {
        prepare_restrictions(&mut report, root, catalog, &cancelled)?;
    }
    prepare_volumes(&mut report, root, catalog, &cancelled)?;
    prepare_decoration(&mut report, root, project, catalog, &cancelled)?;
    prepare_cables(&mut report, root, catalog, &cancelled)?;
    Ok(report)
}

fn prepare_restrictions(
    report: &mut Report, root: &Path, catalog: &Catalog, cancelled: &impl Fn() -> bool,
) -> Result<(), String> {
    let mut restrictions = None;
    step(report, "Preparing no-build zones", cancelled, || {
        restrictions = Some(crate::no_build::prepare(root, &catalog.installation, cancelled)?);
        Ok(())
    })?;
    report.restrictions = restrictions;
    Ok(())
}

fn prepare_volumes(
    report: &mut Report, root: &Path, catalog: &Catalog, cancelled: &impl Fn() -> bool,
) -> Result<(), String> {
    step(report, "Preparing monument volumes", cancelled, || {
        crate::monument_tag_source::prepare_cached(root, catalog, cancelled)?;
        Ok(())
    })
}

fn prepare_decoration(
    report: &mut Report, root: &Path, project: &Project, catalog: &Catalog,
    cancelled: &impl Fn() -> bool,
) -> Result<(), String> {
    let mut decoration = None;
    step(report, "Preparing rocks, cliffs and scenery", cancelled, || {
        decoration = Some(crate::object_extract::prepare_decor_shared(
            root, catalog, project.decoration.as_ref().map(|r| r.source_sha256.as_str()), cancelled,
        )?);
        Ok(())
    })?;
    report.decoration = decoration;
    Ok(())
}

fn prepare_cables(
    report: &mut Report, root: &Path, catalog: &Catalog, cancelled: &impl Fn() -> bool,
) -> Result<(), String> {
    step(report, "Preparing cable connections", cancelled, || {
        crate::scene_wire_source::prepare(root, catalog, cancelled)?;
        Ok(())
    })
}

#[cfg(windows)]
fn preparation_worker_count() -> usize {
    let memory = papyrus_file_id::available_memory().ok();
    loading_workers(
        crate::generation_resources::current().effective_workers(),
        memory.map(|m| m.physical),
        memory.map(|m| m.commit),
    )
}
#[cfg(any(windows, test))]
fn loading_workers(cpu_workers: usize, physical: Option<u64>, commit: Option<u64>) -> usize {
    // Each Unity parser can hold separate decoded bundle buffers. Charge
    // headroom against both RAM and commit, and stay serial if either is unknown.
    const HEADROOM: u64 = 4 * 1024 * 1024 * 1024;
    let available = physical.unwrap_or(0).min(commit.unwrap_or(0));
    ((available / HEADROOM) as usize).min(cpu_workers).clamp(1, 3)
}
#[cfg(any(windows, test))]
type PreparationJob<'a, T> = Box<dyn FnOnce() -> Result<T, String> + Send + 'a>;

#[cfg(windows)]
fn preparation_job<'a>(
    label: &'static str,
    signal: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    user_signal: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    prepare: impl FnOnce(&dyn Fn() -> bool) -> Result<Report, String> + Send + 'a,
) -> Result<PreparationJob<'a, Report>, String> {
    let leases = crate::source_lease::fork()?;
    let signal = signal.clone();
    let user_signal = user_signal.clone();
    Ok(Box::new(move || {
        let _verification = leases.enter();
        let _progress = crate::loading_progress::begin(label, Some(user_signal));
        prepare(&|| signal.load(std::sync::atomic::Ordering::Relaxed))
    }))
}

#[cfg(any(windows, test))]
fn run_preparation_workers<T: Send>(
    cancelled: &impl Fn() -> bool,
    signal: std::sync::Arc<std::sync::atomic::AtomicBool>,
    progress: &crate::loading_progress::Reporter,
    left: impl FnOnce() -> Result<T, String> + Send,
    right: impl FnOnce() -> Result<T, String> + Send,
) -> Result<(T, T), String> {
    let mut results = run_preparation_jobs(
        cancelled, signal, progress, vec![Box::new(left), Box::new(right)],
    )?.into_iter();
    Ok((results.next().unwrap(), results.next().unwrap()))
}

#[cfg(any(windows, test))]
fn run_preparation_jobs<T: Send>(
    cancelled: &impl Fn() -> bool,
    signal: std::sync::Arc<std::sync::atomic::AtomicBool>,
    progress: &crate::loading_progress::Reporter,
    jobs: Vec<PreparationJob<'_, T>>,
) -> Result<Vec<T>, String> {
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::{self, RecvTimeoutError};
    if cancelled() || signal.load(Ordering::Relaxed) {
        return Err("Client preparation cancelled".into());
    }
    let signal_ref = signal.as_ref();
    std::thread::scope(|scope| {
        let (finished, completions) = mpsc::channel();
        let mut workers = Vec::with_capacity(jobs.len());
        for (index, job) in jobs.into_iter().enumerate() {
            let finished = finished.clone();
            match std::thread::Builder::new()
                .name(format!("mapper-prepare-{index}"))
                .spawn_scoped(scope, move || loading_job(job, signal_ref, finished))
            {
                Ok(worker) => workers.push(worker),
                Err(error) => {
                    signal.store(true, Ordering::Relaxed);
                    return Err(format!("Could not start installation loading: {error}"));
                }
            }
        }
        drop(finished);
        let total = workers.len() as u64;
        progress.update("", 0, Some(total), crate::loading_progress::Unit::Items);
        let mut completed = 0;
        while completed < total {
            if cancelled() {
                signal.store(true, Ordering::Relaxed);
            }
            match completions.recv_timeout(Duration::from_millis(20)) {
                Ok(()) => {
                    completed += 1;
                    progress.update("", completed, Some(total), crate::loading_progress::Unit::Items);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        // All children have finished and released their source leases before
        // returning cancellation to the caller. Its callback stays on this thread.
        let result = workers.into_iter().map(|worker| {
            worker.join().unwrap_or_else(|_| Err("Installation loading worker failed".into()))
        }).collect::<Vec<_>>();
        if let Some(error) = result.iter().filter_map(|result| result.as_ref().err())
            .find(|error| error.as_str() != "Client preparation cancelled")
        {
            return Err(error.clone());
        }
        if cancelled() || signal.load(Ordering::Relaxed) {
            return Err("Client preparation cancelled".into());
        }
        result.into_iter().collect()
    })
}
#[cfg(any(windows, test))]
fn loading_job<T>(
    work: impl FnOnce() -> Result<T, String>,
    signal: &std::sync::atomic::AtomicBool,
    finished: std::sync::mpsc::Sender<()>,
) -> Result<T, String> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
        .unwrap_or_else(|_| Err("Installation loading worker failed".into()));
    if result.is_err() {
        signal.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let _ = finished.send(());
    result
}
fn prepare_monuments(
    root: &Path,
    project: &Project,
    catalog: &Catalog,
    bindings: &Bindings,
    include_restrictions: bool,
    separate_scene_data: bool,
    cancelled: impl Fn() -> bool,
) -> Result<Report, String> {
    let mut report = Report::default();
    let mut monuments = None;
    let mut palette = None;
    step(&mut report, "Preparing terrain colors", &cancelled, || {
        palette = Some(std::sync::Arc::new(
            crate::palette_extract::prepare(root, &catalog.installation, &cancelled)?
                .ok_or("Terrain color adapter unavailable")?,
        ));
        Ok(())
    })?;
    report.palette = palette;
    step(&mut report, "Preparing topology layers", &cancelled, || {
        let pin = project.topology_edits.as_ref()
            .filter(|edits| edits.enabled && !edits.strokes.is_empty())
            .map(|edits| edits.source_sha256.as_str());
        crate::spawn_source::Source::prepare(root, catalog, bindings, pin, &cancelled)
    })?;
    let mut labels = None;
    step(&mut report, "Preparing map text", &cancelled, || {
        labels = crate::map_labels::prepare(root, &catalog.installation, &cancelled)?
            .map(std::sync::Arc::new);
        Ok(())
    })?;
    report.map_labels = labels;
    if include_restrictions && !separate_scene_data {
        prepare_restrictions(&mut report, root, catalog, &cancelled)?;
    }
    step(
        &mut report,
        "Preparing monument generation",
        &cancelled,
        || {
            let mut source = if let Some(rules) = &project.monument_rules {
                crate::monument_source::Source::cached(rules, catalog, bindings, &cancelled)?
            } else {
                match crate::monument_source::Source::discover_rules(catalog, bindings, &cancelled)?
                {
                    Some(source) => source,
                    None => {
                        crate::monument_extract::prepare_environment(root, catalog, bindings, &cancelled)?
                            .ok_or("Monument adapter unavailable")?
                    }
                }
            };
            if !crate::monument_extract::has_environment_meshes(&source) {
                crate::monument_extract::prepare_environment(root, catalog, bindings, &cancelled)?
                    .ok_or("Monument collision capture cancelled")?;
            }
            if source.coastal_profile.is_none() {
                source.prepare_coastal(catalog, root, &cancelled)?;
            }
            if source.stage_source.is_none() {
                source.prepare_stages(catalog, root, &cancelled)?;
            }
            monuments = Some(source);
            Ok(())
        },
    )?;
    let mut tunnel_markers = None;
    step(
        &mut report,
        "Preparing underground rail modules",
        &cancelled,
        || {
            if let Some(monuments) = &monuments
                && let Some(tunnels) = crate::tunnel_source::Source::prepare(
                    root, catalog, monuments, &cancelled,
                )?
                && let Some(upwards) = tunnels.selectors.get("UpwardsFolder")
            {
                tunnel_markers = Some(TunnelMarkerOffsets {
                    catalog_sha256: catalog.sha256.clone(),
                    offsets: upwards.prefabs.iter().filter_map(|path| {
                        let [x, _, z] = tunnels.prefabs.get(path)?.surface_offset?;
                        Some((path.clone(), [x, z]))
                    }).collect(),
                });
            }
            Ok(())
        },
    )?;
    report.tunnel_markers = tunnel_markers;
    step(
        &mut report,
        "Preparing underwater lab modules",
        &cancelled,
        || {
            if let Some(monuments) = &mut monuments {
                monuments.dungeon_source = if let Some(binding) = &monuments.dungeon_source {
                    Some(
                        crate::dungeon_source::Source::cached(
                            &binding.source_sha256,
                            root,
                            catalog,
                            monuments,
                            &cancelled,
                        )?
                        .binding(),
                    )
                } else {
                    crate::dungeon_source::Source::prepare(root, catalog, monuments, &cancelled)?
                        .map(|s| s.binding())
                };
            }
            Ok(())
        },
    )?;
    step(
        &mut report,
        "Preparing offshore monuments",
        &cancelled,
        || {
            if let Some(monuments) = &mut monuments {
                monuments.offshore_source = if let Some(binding) = &monuments.offshore_source {
                    crate::offshore_generation::cached(binding, root, catalog, &cancelled)?;
                    Some(binding.clone())
                } else {
                    crate::offshore_generation::prepare(root, catalog, &cancelled)?
                };
            }
            Ok(())
        },
    )?;
    if !separate_scene_data {
        prepare_volumes(&mut report, root, catalog, &cancelled)?;
    }
    report.monuments = monuments.map(|source| {
        crate::monument_source::Prepared::verified(source, catalog, bindings)
    });
    Ok(report)
}
fn prepare_paths(
    root: &Path,
    project: &Project,
    catalog: &Catalog,
    bindings: &Bindings,
    separate_scene_data: bool,
    cancelled: impl Fn() -> bool,
) -> Result<Report, String> {
    let mut report = Report::default();
    step(
        &mut report,
        "Preparing offshore generation data",
        &cancelled,
        || {
            crate::offshore_source::Source::prepare(root, catalog, &cancelled)?;
            Ok(())
        },
    )?;
    let mut infrastructure = None;
    let mut roads = None;
    step(
        &mut report,
        "Preparing roads and dirt trails",
        &cancelled,
        || {
            let (saved, captured) = crate::road_extract::prepare_startup(
                root,
                catalog,
                project.roads.as_ref().map(|r| r.source_sha256.as_str()),
                &cancelled,
            )?;
            let candidate = if project.roads.is_some() {
                let candidate = crate::infrastructure::Source::discover(catalog, None, &cancelled)?;
                (candidate.profile.revision > saved.profile.revision).then_some(candidate)
            } else {
                None
            };
            let (source, previous_source, captured) = match candidate {
                Some(source) => (source, Some(saved), None),
                None => (saved, None, captured),
            };
            infrastructure = Some(source);
            let source = infrastructure.as_ref().unwrap();
            let (surface, ordered) = if let Some(captured) = captured {
                captured.into_startup_inputs(source)?
            } else {
                let surface = crate::road_extract::prepare_surface(root, source, &cancelled)?
                    .ok_or("Road surfaces unavailable")?;
                let ordered =
                    crate::road_extract::prepare_ordered(root, catalog, source, None, &cancelled)?
                        .ok_or("Road connectors unavailable")?;
                (surface, ordered)
            };
            let roadside = crate::roadside_source::prepare(root, catalog, &cancelled)?
                .ok_or("Roadside generation unavailable")?;
            let paint = crate::road_paint_source::Profile::for_source(&roadside)?
                .ok_or("No matching roadside paint policy")?;
            crate::road_terrain_source::Profile::for_paint(&paint)?
                .ok_or("No matching roadside terrain policy")?;
            let joined = crate::road_cost_source::Profile::bundled_joined(source)?;
            roads = Some(ReadyRoads {
                source: infrastructure.take().unwrap(),
                previous_source,
                surface: surface.sha256,
                roadside: roadside.sha256,
                joined,
                ordered: crate::source_road_planning::Policy::bind(&ordered),
            });
            Ok(())
        },
    )?;
    let mut rails = None;
    step(
        &mut report,
        "Preparing rails and track templates",
        &cancelled,
        || {
            let source = roads
                .as_ref()
                .map(|r| &r.source)
                .or(infrastructure.as_ref())
                .ok_or("Road infrastructure unavailable")?;
            let prepared = crate::rail_extract::prepare_cached(
                root,
                catalog,
                source,
                project.rails.as_ref(),
                &cancelled,
            )?;
            rails = Some(ReadyRails {
                root: root.to_owned(),
                catalog: catalog.sha256.clone(),
                bindings: bindings.sha256.clone(),
                prepared,
            });
            Ok(())
        },
    )?;
    report.rails = rails;
    let mut rivers = None;
    step(&mut report, "Preparing rivers", &cancelled, || {
        let prepared = crate::river_extract::prepare(
            root,
            &catalog.installation,
            project.rivers.as_ref(),
            &cancelled,
        )?
        .ok_or("River adapter unavailable")?;
        rivers = Some(prepared.source.recipe());
        Ok(())
    })?;
    let mut river_objects = None;
    step(&mut report, "Preparing river objects", &cancelled, || {
        let source = roads
            .as_ref()
            .map(|r| &r.source)
            .or(infrastructure.as_ref())
            .ok_or("Path infrastructure unavailable")?;
        let objects = crate::object_extract::prepare_rivers(
            root,
            source,
            project
                .rivers
                .as_ref()
                .and_then(|r| r.objects.as_ref())
                .map(|r| r.source_sha256.as_str()),
            &cancelled,
        )?;
        if let Some(recipe) = &mut rivers {
            recipe.objects = Some(crate::river_objects::Recipe::bind(&objects, source)?);
        }
        river_objects = Some(objects);
        Ok(())
    })?;
    report.river_objects = river_objects;
    report.rivers = rivers;
    if !separate_scene_data {
        prepare_decoration(&mut report, root, project, catalog, &cancelled)?;
    }
    let mut road_objects = None;
    step(
        &mut report,
        "Preparing road objects",
        &cancelled,
        || {
            let source = roads
                .as_ref()
                .map(|r| &r.source)
                .or(infrastructure.as_ref())
                .ok_or("Road infrastructure unavailable")?;
            road_objects = Some(crate::object_extract::prepare_roads(
                root,
                source,
                project
                    .objects
                    .as_ref()
                    .map(|r| r.road_source_sha256.as_str()),
                &cancelled,
            )?);
            Ok(())
        },
    )?;
    step(&mut report, "Preparing roadside pipes", &cancelled, || {
        let infrastructure = roads
            .as_ref()
            .map(|ready| &ready.source)
            .or(infrastructure.as_ref())
            .ok_or("Road infrastructure unavailable")?;
        let flags = infrastructure
            .profile
            .topology_flags
            .as_ref()
            .ok_or("Road topology roles unavailable")?;
        crate::water_treatment_source::prepare(
            root,
            catalog,
            road_objects
                .as_ref()
                .ok_or("Road object workflow unavailable")?,
            flags,
            infrastructure.profile.protected_topology as u32,
            &cancelled,
        )?;
        Ok(())
    })?;
    step(&mut report, "Preparing substations and powerlines", &cancelled, || {
        let infrastructure = roads.as_ref().map(|ready| &ready.source)
            .or(infrastructure.as_ref()).ok_or("Road infrastructure unavailable")?;
        crate::powerline_source::prepare(
            root, catalog, road_objects.as_ref().ok_or("Road object workflow unavailable")?,
            infrastructure.profile.topology_flags.as_ref().ok_or("Road topology roles unavailable")?,
            infrastructure.profile.protected_topology as u32, &cancelled,
        )?;
        Ok(())
    })?;
    if !separate_scene_data {
        prepare_cables(&mut report, root, catalog, &cancelled)?;
    }
    let mut uniform_cliffs = None;
    step(&mut report, "Preparing hill cliffs", &cancelled, || {
        let road = road_objects.as_ref().ok_or("Road object workflow unavailable")?;
        let source = crate::uniform_cliff_source::prepare(
            root,
            catalog,
            road,
            &cancelled,
        )?;
        uniform_cliffs = Some(ReadyCliffs {
            root: root.to_owned(),
            catalog: catalog.sha256.clone(),
            road_source: road.sha256.clone(),
            source: source.sha256().to_owned(),
            coastal: None,
            vines: None,
        });
        Ok(())
    })?;
    step(&mut report, "Preparing coastal cliffs", &cancelled, || {
        let source = crate::coastal_cliff_source::prepare(
            root,
            catalog,
            road_objects.as_ref().ok_or("Road object workflow unavailable")?,
            &cancelled,
        )?;
        if let Some(ready) = uniform_cliffs.as_mut() {
            ready.coastal = Some(source.sha256().to_owned());
        }
        Ok(())
    })?;
    step(&mut report, "Preparing vine trees", &cancelled, || {
        let source = crate::jungle_vine_source::prepare(
            root, catalog,
            road_objects.as_ref().ok_or("Road object workflow unavailable")?,
            &cancelled,
        )?;
        if let Some(ready) = uniform_cliffs.as_mut() {
            ready.vines = Some(source.sha256().to_owned());
        }
        Ok(())
    })?;
    report.uniform_cliffs = uniform_cliffs;
    report.road_object_sha256 = road_objects.as_ref().map(|source| source.sha256.clone());
    report.road_objects = road_objects;
    let mut identities = None;
    step(
        &mut report,
        "Preparing map export identities",
        &cancelled,
        || {
            let source = crate::prefab_identities::Source::prepare(root, catalog, &cancelled)?
                .ok_or("Map export preparation cancelled")?;
            identities = Some(std::sync::Arc::new(ReadyIdentities {
                addons: crate::monument_addons::Dependencies::new(&source, catalog),
                source,
            }));
            Ok(())
        },
    )?;
    report.identities = identities;
    report.roads = roads;
    Ok(report)
}

#[cfg(test)]
mod tests {
    #[test]
    fn prepared_cliffs_bind_once_and_reject_stale_installations_roads_and_legacy_recipes() {
        let ready = ReadyCliffs {
            root: PathBuf::from("selected-rust"),
            catalog: "a".repeat(64),
            road_source: "b".repeat(64),
            source: "c".repeat(64),
            coastal: Some("f".repeat(64)),
            vines: Some("a".repeat(64)),
        };
        let mut project = crate::Project::default();
        assert!(!ready.apply(&mut project, &ready.root, &ready.catalog));
        project.objects = Some(crate::object_generation::Recipe::new(
            ready.road_source.clone(),
        ));
        let before = project.clone();
        assert!(!ready.apply(&mut project, Path::new("another-rust"), &ready.catalog));
        assert!(!ready.apply(&mut project, &ready.root, &"d".repeat(64)));
        assert_eq!(project, before);
        project.objects.as_mut().unwrap().engine = crate::object_generation::LEGACY_VERSION.into();
        assert!(!ready.apply(&mut project, &ready.root, &ready.catalog));
        project.objects.as_mut().unwrap().engine = crate::object_generation::VERSION.into();
        project.objects.as_mut().unwrap().road_source_sha256 = "e".repeat(64);
        assert!(!ready.apply(&mut project, &ready.root, &ready.catalog));
        project
            .objects
            .as_mut()
            .unwrap()
            .road_source_sha256
            .clone_from(&ready.road_source);
        assert!(ready.apply(&mut project, &ready.root, &ready.catalog));
        assert_eq!(
            project
                .objects
                .as_ref()
                .unwrap()
                .uniform_cliffs_source_sha256
                .as_deref(),
            Some(ready.source.as_str())
        );
        assert_eq!(project.objects.as_ref().unwrap().coastal_cliffs_source_sha256, ready.coastal);
        assert_eq!(project.objects.as_ref().unwrap().jungle_vines_source_sha256, ready.vines);
        let bound = project.clone();
        assert!(!ready.apply(&mut project, &ready.root, &ready.catalog));
        assert_eq!(project, bound);
    }
    use super::*;
    #[cfg(windows)]
    #[test]
    fn parallel_hashes_keep_exact_receipts_and_deny_write_leases_in_the_parent_scope() {
        use sha2::{Digest, Sha256};
        let root = tempfile::tempdir().unwrap();
        let paths = [CORE_SOURCES[1], CORE_SOURCES[4]];
        let payloads: [&[u8]; 2] = [b"bundle receipt", b"assembly receipt"];
        for (path, payload) in paths.into_iter().zip(payloads) {
            let file = root.path().join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, payload).unwrap();
        }
        {
            let _scope = crate::installation::VerificationScope::default();
            verify_core_sources(root.path(), 2, &|| false).unwrap();
            let receipts = crate::source_lease::receipts();
            assert_eq!(receipts.len(), 2);
            for (path, payload) in paths.into_iter().zip(payloads) {
                let sha = format!("{:x}", Sha256::digest(payload));
                assert!(receipts.iter().any(|r| r["sha256"] == sha));
                assert!(std::fs::write(root.path().join(path), b"changed").is_err());
            }
            verify_core_sources(root.path(), 2, &|| false).unwrap();
            assert_eq!(crate::source_lease::receipts(), receipts);
        }
        std::fs::write(root.path().join(paths[0]), b"changed").unwrap();
        let _scope = crate::installation::VerificationScope::default();
        verify_core_sources(root.path(), 2, &|| false).unwrap();
        let changed = format!("{:x}", Sha256::digest(b"changed"));
        assert!(crate::source_lease::receipts().iter().any(|r| r["sha256"] == changed));
    }

    #[test]
    fn loading_concurrency_respects_cpu_and_memory_headroom() {
        let headroom = 8 * 1024 * 1024 * 1024;
        assert_eq!(loading_workers(2, Some(headroom), Some(headroom)), 2);
        assert_eq!(loading_workers(1024, Some(headroom), Some(headroom)), 2);
        assert_eq!(loading_workers(1, Some(headroom), Some(headroom)), 1);
        assert_eq!(loading_workers(2, Some(headroom - 1), Some(headroom)), 1);
        assert_eq!(loading_workers(2, Some(headroom), Some(headroom - 1)), 1);
        assert_eq!(loading_workers(2, None, Some(headroom)), 1);
        assert_eq!(loading_workers(2, Some(headroom), None), 1);
        let three = 12 * 1024 * 1024 * 1024;
        assert_eq!(loading_workers(3, Some(three), Some(three)), 3);
        assert_eq!(loading_workers(2, Some(three), Some(three)), 2);
        assert_eq!(loading_workers(3, Some(three), Some(three - 1)), 2);
    }
    #[test]
    fn parallel_preparation_keeps_report_order_and_stage_errors() {
        let signal = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let progress =
            crate::loading_progress::begin("Parallel preparation order test", Some(signal.clone()));
        let barrier = std::sync::Barrier::new(2);
        let (left, right) = run_preparation_workers(
            &|| false,
            signal,
            &progress.reporter(),
            || {
                barrier.wait();
                let mut report = Report::default();
                step(
                    &mut report,
                    "First",
                    &|| false,
                    || Err("Unavailable".into()),
                )?;
                step(&mut report, "Second", &|| false, || Ok(()))?;
                Ok(report)
            },
            || {
                barrier.wait();
                let mut report = Report::default();
                step(&mut report, "Third", &|| false, || Ok(()))?;
                Ok(report)
            },
        )
        .unwrap();
        let report = combine_reports(left, right);
        assert_eq!(
            report.steps.iter().map(|s| s.name).collect::<Vec<_>>(),
            ["First", "Second", "Third"]
        );
        assert_eq!(report.steps[0].error.as_deref(), Some("Unavailable"));
        assert!(report.steps[1..].iter().all(|s| s.error.is_none()));
    }
    #[test]
    fn cancellation_polls_on_owner_and_joins_all_three_workers() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        };
        let signal = Arc::new(AtomicBool::new(false));
        let started = AtomicUsize::new(0);
        let stopped = AtomicUsize::new(0);
        let callbacks = std::cell::Cell::new(0);
        let owner = std::thread::current().id();
        let cancelled = || {
            assert_eq!(owner, std::thread::current().id());
            callbacks.set(callbacks.get() + 1);
            started.load(Ordering::SeqCst) == 3
        };
        let work = || {
            started.fetch_add(1, Ordering::SeqCst);
            while !signal.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(1));
            }
            stopped.fetch_add(1, Ordering::SeqCst);
            Ok(Report::default())
        };
        let progress = crate::loading_progress::begin(
            "Parallel preparation cancellation test",
            Some(signal.clone()),
        );
        assert!(
            run_preparation_jobs(
                &cancelled, signal.clone(), &progress.reporter(),
                vec![Box::new(work), Box::new(work), Box::new(work)],
            ).is_err()
        );
        assert_eq!(stopped.load(Ordering::SeqCst), 3);
        assert!(callbacks.get() >= 2);
    }
    #[test]
    fn worker_panic_cancels_and_joins_its_peer() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let signal = Arc::new(AtomicBool::new(false));
        let stopped = AtomicBool::new(false);
        let progress =
            crate::loading_progress::begin("Parallel preparation panic test", Some(signal.clone()));
        let result = run_preparation_workers(
            &|| false,
            signal.clone(),
            &progress.reporter(),
            || panic!("test worker failure"),
            || {
                while !signal.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                stopped.store(true, Ordering::SeqCst);
                Ok(Report::default())
            },
        );
        assert_eq!(
            result.err().as_deref(),
            Some("Installation loading worker failed")
        );
        assert!(stopped.load(Ordering::SeqCst));
    }
    #[test]
    fn path_worker_error_survives_monument_worker_cancellation() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let signal = Arc::new(AtomicBool::new(false));
        let user_signal = Arc::new(AtomicBool::new(false));
        let progress = crate::loading_progress::begin(
            "Parallel preparation error test",
            Some(user_signal.clone()),
        );
        let result = run_preparation_workers::<Report>(
            &|| false,
            signal.clone(),
            &progress.reporter(),
            || {
                while !signal.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err("Client preparation cancelled".into())
            },
            || Err("Path worker failure".into()),
        );
        assert_eq!(result.err().as_deref(), Some("Path worker failure"));
        assert!(!user_signal.load(Ordering::Relaxed));
    }
    #[test]
    fn unsupported_stages_are_visible_and_do_not_skip_other_stages() {
        let mut report = Report::default();
        step(&mut report, "Unavailable", &|| false, || {
            Err("No adapter".into())
        })
        .unwrap();
        step(&mut report, "Available", &|| false, || Ok(())).unwrap();
        assert_eq!(report.steps.len(), 2);
        assert_eq!(report.steps[0].error.as_deref(), Some("No adapter"));
        assert!(report.steps[1].error.is_none());
    }
    #[test]
    fn cancellation_never_publishes_a_completed_stage() {
        let mut report = Report::default();
        assert!(
            step(&mut report, "Cancelled", &|| true, || panic!(
                "Work must not start"
            ))
            .is_err()
        );
        let stopped = std::cell::Cell::new(false);
        assert!(
            step(&mut report, "Cancelled", &|| stopped.get(), || {
                stopped.set(true);
                Ok(())
            })
            .is_err()
        );
        assert!(report.steps.is_empty());
    }
}
