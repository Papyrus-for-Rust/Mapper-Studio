//! Console entry point; kept separate from the Windows GUI subsystem executable.
use mapper_studio::{
    Project, headless, installation,
    loading_progress::{self, Snapshot},
    native_preview, native_profile, plugins,
    profile::{self, Profile},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, ExitCode},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const HELP: &str = "Mapper for Rust CLI\n\nUsage:\n  mapper --help | --version\n  mapper gui [PROJECT]\n  mapper project new OUTPUT [--seed N] [--size METRES] [--rust-root DIR] [--ocean] [--native-profile FILE] [--force]\n  mapper project bind-native-profile PROJECT --rust-root DIR --profile FILE --output PROJECT [--force]\n  mapper project bind-gpu-erosion PROJECT --output PROJECT [--force]\n  mapper project use-cpu-erosion PROJECT --output PROJECT [--force]\n  mapper project auto-erosion PROJECT --output PROJECT [--cpu-workers N] [--gpu-memory-mib N] [--force]\n  mapper project bind-terrain-graph PROJECT --graph FILE --output PROJECT [--force]\n  mapper project clear-terrain-graph PROJECT --output PROJECT [--force]\n  mapper native-profile inspect FILE\n  mapper project add-monument-addon PROJECT --source FILE [--anchor INDEX] --output PROJECT [--force]\n  mapper project bind-assets PROJECT --rust-root DIR --catalog FILE --bindings FILE\n                 --output PROJECT [--force]\n  mapper project bind-roads PROJECT --rust-root DIR --catalog FILE --source FILE --output PROJECT [--surfaces SHA256] [--main-ring | --no-main-ring] [--trails | --no-trails] [--force]\n  mapper project inspect-monuments PROJECT --rust-root DIR --catalog FILE --bindings FILE --source FILE [--details]\n  mapper project bind-monuments PROJECT --rust-root DIR --catalog FILE --bindings FILE --source FILE --output PROJECT [--force]\n  mapper project prepare-rivers PROJECT --rust-root DIR --output PROJECT [--force]\n  mapper project disable-rivers PROJECT --output PROJECT [--force]\n  mapper project prepare-rails PROJECT --rust-root DIR --catalog FILE --bindings FILE --settings FILE --output PROJECT [--force]\n  mapper project bind-rails PROJECT --rust-root DIR --catalog FILE --bindings FILE --source FILE --meshes FILE --settings FILE --output PROJECT [--force]\n  mapper project bind-objects PROJECT --rust-root DIR --catalog FILE --bindings FILE --road-source SHA256 --output PROJECT [--force]\n  mapper project bind-decor PROJECT --rust-root DIR --catalog FILE --source FILE --output PROJECT [--force]\n  mapper project inspect-object-workflow PROJECT --rust-root DIR --catalog FILE --infrastructure FILE --source FILE --decoration-source FILE\n  mapper project inspect-road-objects PROJECT --rust-root DIR --catalog FILE --infrastructure FILE --source FILE [--terrain-bundle DIR [--effects-output NEW_DIRECTORY | --live-effects-output NEW_DIRECTORY]] [--details]\n  mapper project inspect-rails PROJECT --rust-root DIR --catalog FILE --source FILE [--infrastructure FILE] [--tracks TRACKS_JSON] [--meshes MESHES_JSON] [--curve POINTS_JSON [--mesh-deform QUERY_JSON]] [--terrain-bundle DIR [--route QUERY_JSON] [--ring RING_JSON [--approaches APPROACH_JSON [--bed BED_JSON [--bed-output NEW_DIRECTORY]]]]] [--cached]\n  mapper project inspect-water PROJECT --rust-root DIR --catalog FILE [--collision-cache FILE] [--details]\n  mapper content inspect RUSTEDIT_PREFAB\n  mapper content list-custom-monuments\n  mapper project add-custom-monument PROJECT (--version ID | --package FILE) --position X,Y,Z [--yaw DEGREES] --output PROJECT [--force]\n  mapper project validate PROJECT\n  mapper project inspect PROJECT\n  mapper installation reader\n  mapper installation prepare RUST_ROOT [--project PROJECT]\n  mapper installation inspect RUST_ROOT\n  mapper installation inspect-offshore RUST_ROOT --catalog FILE [--source CAPTURE]\n  mapper recipe export PROJECT PROFILE [--force]\n  mapper recipe import PROFILE PROJECT [--force]\n  mapper recipe import-rustmaps PROFILE PROJECT [--base PROJECT] [--force]\n  mapper generate PROJECT [--output NEW_DIRECTORY] [--map FILE] [--rust-root DIR]\n                  [--worker FILE] [--cpu-workers N] [--gpu-memory-mib N] [--catalog FILE --bindings FILE | --no-compose]\n  mapper plugins inspect MANIFEST\n  mapper plugins check MANIFEST ACTION --project PROJECT [--parameters JSON_FILE] [--timeout SECONDS]\n  mapper plugins run MANIFEST ACTION --project PROJECT --output PROJECT\n                 [--parameters JSON_FILE] [--timeout SECONDS] [--force]\n\nUse --output for a preview bundle, --map for a Rust map container, or both. Server loading and complete world-generation coverage remain under verification.\nNative recipes use the matching Rust folder and Mapper's prepared asset cache. Use both asset file options to choose a pair explicitly.\nPlugins run as local processes with your user permissions, only when explicitly invoked.\nJSON results go to stdout; progress/errors go to stderr. Ctrl+C cancels generation/plugins.\nExit codes: 0 success, 1 operation failed, 2 invalid command, 130 cancelled.\n";

#[derive(Debug)]
struct Error {
    code: u8,
    message: String,
}
fn failure(message: impl ToString) -> Error {
    Error {
        code: 1,
        message: message.to_string(),
    }
}
fn usage(message: impl ToString) -> Error {
    Error {
        code: 2,
        message: message.to_string(),
    }
}
#[derive(Default)]
struct Args {
    positionals: Vec<OsString>,
    flags: BTreeMap<String, Option<OsString>>,
}
impl Args {
    fn parse(args: &[OsString], allowed: &[&str], booleans: &[&str]) -> Result<Self, Error> {
        let mut result = Self::default();
        let mut at = 0;
        let mut literal = false;
        while at < args.len() {
            let arg = &args[at];
            at += 1;
            if !literal && arg == "--" {
                literal = true;
                continue;
            }
            if !literal && arg.to_string_lossy().starts_with('-') {
                let flag = arg
                    .to_str()
                    .ok_or_else(|| usage("Invalid option encoding"))?;
                if !allowed.contains(&flag) {
                    return Err(usage(format!("Unknown option: {flag}")));
                }
                if [
                    "--no-constrained-road-smoothing",
                    "--no-stage-filters",
                    "--no-authored-earthwork",
                    "--no-tier-retries",
                    "--no-connected-exits",
                    "--no-ring-recovery",
                    "--no-road-topology",
                    "--no-ring-costmap",
                    "--no-access-costmap",
                    "--no-terrain-road-entrances",
                    "--no-global-road-earthwork",
                    "--no-staged-roads",
                    "--legacy-road-layout",
                ]
                .contains(&flag)
                {
                    return Err(usage(format!(
                        "{flag} is no longer supported: generation correctness rules are automatic"
                    )));
                }
                if result.flags.contains_key(flag) {
                    return Err(usage(format!("Duplicate option: {flag}")));
                }
                let value = if booleans.contains(&flag) {
                    None
                } else {
                    let value = args
                        .get(at)
                        .filter(|v| !v.to_string_lossy().starts_with("--"))
                        .ok_or_else(|| usage(format!("Missing value for {flag}")))?;
                    at += 1;
                    Some(value.clone())
                };
                result.flags.insert(flag.into(), value);
            } else {
                result.positionals.push(arg.clone());
            }
        }
        Ok(result)
    }
    fn count(&self, n: usize) -> Result<(), Error> {
        if self.positionals.len() == n {
            Ok(())
        } else {
            Err(usage(format!(
                "Expected {n} positional arguments; run mapper --help"
            )))
        }
    }
    fn path(&self, flag: &str) -> Option<PathBuf> {
        self.flags
            .get(flag)
            .and_then(Option::as_ref)
            .map(PathBuf::from)
    }
    fn number(&self, flag: &str) -> Result<Option<u32>, Error> {
        self.flags
            .get(flag)
            .and_then(Option::as_ref)
            .map(|v| {
                v.to_str()
                    .and_then(|s| s.parse().ok())
                    .ok_or_else(|| usage(format!("{flag} requires an unsigned integer")))
            })
            .transpose()
    }
    fn has(&self, flag: &str) -> bool {
        self.flags.contains_key(flag)
    }
}
fn save(path: &Path, value: &impl serde::Serialize, force: bool) -> Result<(), Error> {
    if path.exists() && !force {
        return Err(failure(
            "Output already exists; choose another file or pass --force",
        ));
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(failure)?;
    profile::write_document(path, value, force).map_err(failure)
}
fn text(value: &OsStr) -> Result<&str, Error> {
    value
        .to_str()
        .ok_or_else(|| usage("Command/action must be UTF-8"))
}
fn save_project_edit(
    source: &Path,
    output: &Path,
    project: &Project,
    force: bool,
    cancel: &AtomicBool,
) -> Result<(), Error> {
    if output.exists() && !force {
        return Err(failure("Output already exists; choose another file or pass --force"));
    }
    let parent = output.parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(failure)?;
    if !mapper_studio::project_bundle::rewrite_recipe(
        source, output, project.clone(), force, || cancel.load(Ordering::Relaxed),
    ).map_err(failure)? {
        save(output, project, force)?;
    }
    Ok(())
}

fn compute_limits(parsed: &Args) -> Result<mapper_studio::generation_resources::Limits, Error> {
    let mut resources = mapper_studio::generation_resources::current();
    if let Some(workers) = parsed.number("--cpu-workers")? {
        if !(1..=1024).contains(&workers) {
            return Err(usage("--cpu-workers requires a count from 1 to 1024"));
        }
        resources.cpu_workers = workers as usize;
    }
    if let Some(memory) = parsed.number("--gpu-memory-mib")? {
        if !(32..=2048).contains(&memory) {
            return Err(usage("--gpu-memory-mib requires a value from 32 to 2048"));
        }
        resources.gpu_memory_mib = memory as usize;
    }
    Ok(resources)
}

fn save_generation_preview(
    generated: &mut headless::Generated,
    output: &Path,
    map_requested: bool,
    cancel: &AtomicBool,
    timing: impl Fn(&str),
) -> Result<Value, Error> {
    let incomplete = if map_requested {
        use mapper_studio::generation_validation;
        let issue = generation_validation::required_monuments_issue(
            &generated.recipe,
            &generated.solution,
        )
        .or_else(|| match &generated.composed {
            Some(maps) => {
                generation_validation::validate(&generated.recipe, maps, &generated.solution).err()
            }
            None => Some("Map export requires the accepted composed world".into()),
        });
        if let Some(issue) = &issue {
            if !generated.warnings.contains(issue) {
                generated.warnings.push(issue.clone());
            }
        } else {
            timing("export_validated");
        }
        issue
    } else {
        None
    };
    // Persist the effective recipe, including resolved installation defaults.
    let manifest =
        headless::save_bundle(&generated.recipe, generated, output, cancel).map_err(|error| {
            match &incomplete {
                Some(issue) => failure(format!("{issue}\nCould not save preview. {error}")),
                None => failure(error),
            }
        })?;
    timing("bundle_saved");
    if let Some(issue) = incomplete {
        return Err(with_saved_preview(failure(issue), output));
    }
    Ok(manifest)
}

fn with_saved_preview(mut error: Error, output: &Path) -> Error {
    error.message.push_str(&format!(
        "\nPreview and recipe saved to {}",
        output.display()
    ));
    error
}

fn erosion_milestone(job: &Snapshot, last: &mut Option<u8>) -> Option<(u64, u64)> {
    if job.stage != "Simulating erosion" {
        return None;
    }
    let total = job.total.filter(|&total| total > 0)?;
    let completed = job.completed.min(total);
    // Show the first completed iteration before the first ten-percent boundary.
    let milestone = u8::from(completed > 0) + (u128::from(completed) * 10 / u128::from(total)) as u8;
    if *last == Some(milestone) {
        return None;
    }
    *last = Some(milestone);
    Some((completed, total))
}

fn monitor_loading<T>(mut report: impl FnMut() + Send, work: impl FnOnce() -> T) -> T {
    std::thread::scope(|scope| {
        let (finished, stop) = std::sync::mpsc::channel::<()>();
        let _ = std::thread::Builder::new()
            .name("mapper-cli-progress".into())
            .spawn_scoped(scope, move || {
                while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
                    stop.recv_timeout(Duration::from_secs(1))
                {
                    report();
                }
            });
        let result = work();
        drop(finished);
        result
    })
}

fn preparation_progress(jobs: &[Snapshot]) -> String {
    let mut stages = std::collections::BTreeMap::<&str, Vec<&str>>::new();
    for job in jobs {
        if job.stage == "Preparing generation data" && jobs.len() > 1 {
            continue;
        }
        let items = stages.entry(&job.stage).or_default();
        let item = job.item.rsplit(['/', '\\']).next().unwrap_or_default();
        if !item.is_empty() && !items.contains(&item) {
            items.push(item);
        }
    }
    stages
        .into_iter()
        .map(|(stage, items)| {
            if items.is_empty() {
                stage.to_owned()
            } else {
                format!("{stage} · {}", items.join(", "))
            }
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

fn generate_with_progress<T>(
    monitor_erosion: bool,
    generate: impl FnOnce(fn(&str, f32)) -> Result<Option<T>, String>,
) -> Result<T, Error> {
    let progress = |label: &str, _: f32| eprintln!("{label}");
    let result = if monitor_erosion {
        let mut last = None;
        monitor_loading(
            || {
                for job in loading_progress::active() {
                    if let Some((done, total)) = erosion_milestone(&job, &mut last) {
                        eprintln!("Simulating erosion {done}/{total} iterations");
                    }
                }
            },
            || generate(progress),
        )
    } else {
        generate(progress)
    };
    result
        .map_err(failure)?
        .ok_or_else(|| failure("Generation cancelled"))
}

fn write_generated_map(
    input: mapper_studio::map_export::Input<'_>,
    destination: &Path,
    cancel: &AtomicBool,
) -> Result<(), Error> {
    eprintln!("Saving game map container");
    if !mapper_studio::map_export::write(input, destination, false, || cancel.load(Ordering::Relaxed))
        .map_err(failure)?
    {
        return Err(failure("Map export cancelled"));
    }
    Ok(())
}

fn generate(args: &[OsString], cancel: &AtomicBool) -> Result<Value, Error> {
    let parsed = Args::parse(
        &args[1..],
        &[
            "--output",
            "--rust-root",
            "--worker",
            "--cpu-workers",
            "--gpu-memory-mib",
            "--catalog",
            "--bindings",
            "--no-compose",
            "--map",
        ],
        &["--no-compose"],
    )?;
    parsed.count(1)?;
    let output = parsed.path("--output");
    let map_output = parsed.path("--map");
    if output.is_none() && map_output.is_none() {
        return Err(usage("generate requires --output NEW_DIRECTORY or --map FILE"));
    }
    if output.as_ref().is_some_and(|path| {
        path.exists() || path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("map"))
    }) {
        return Err(failure(
            "Generation requires a new preview directory. Use --map FILE for game container output.",
        ));
    }
    let project = Project::load(Path::new(&parsed.positionals[0])).map_err(failure)?;
    let options = headless::Options {
        rust_root: parsed.path("--rust-root"),
        worker: parsed.path("--worker"),
        catalog: parsed.path("--catalog"),
        bindings: parsed.path("--bindings"),
        skip_composition: parsed.has("--no-compose"),
    };
    if options.catalog.is_some() != options.bindings.is_some() {
        return Err(usage("Provide both --catalog and --bindings"));
    }
    if let Some(path) = &map_output {
        if path.exists() {
            return Err(failure("Map output already exists"));
        }
        if options.skip_composition || options.rust_root.is_none() {
            return Err(usage("--map requires composed native generation with --rust-root"));
        }
    }
    mapper_studio::generation_resources::configure(compute_limits(&parsed)?).map_err(failure)?;
    let started = std::env::var_os("MAPPER_COMPOSITION_TIMINGS")
        .map(|_| std::time::Instant::now());
    let timing = |stage: &str| {
        if let Some(started) = started {
            let event = json!({"stage": stage, "elapsed_ms": started.elapsed().as_secs_f64() * 1000.});
            eprintln!("MAPPER_CLI_TIMING {event}");
        }
    };
    // Keep source leases through .map export so prefab resolution uses the
    // same installation snapshot and catalog as the accepted composition.
    let _sources = map_output.as_ref().map(|_| installation::VerificationScope::default());
    let _identities = mapper_studio::prefab_identities::Scope::default();
    let manifest = if let Some(directory) = &output {
        let composed =
            native_preview::is_native(&project.preview_engine) && !options.skip_composition;
        let (mut generated, catalog) = generate_with_progress(composed, |progress| {
            headless::generate_with_catalog(&project, &options, cancel, progress)
        })?;
        timing("generated");
        let manifest = save_generation_preview(
            &mut generated,
            directory,
            map_output.is_some(),
            cancel,
            timing,
        )?;
        if let Some(path) = &map_output {
            let export = || {
                let catalog = catalog.as_ref().ok_or_else(|| {
                    failure("Map export requires the verified generation catalog")
                })?;
                let maps = generated
                    .composed
                    .as_ref()
                    .ok_or_else(|| failure("Map export requires the accepted composed world"))?;
                write_generated_map(
                    mapper_studio::map_export::Input {
                        project: &generated.recipe,
                        maps,
                        solution: &generated.solution,
                        root: options.rust_root.as_deref().unwrap(),
                        catalog,
                    },
                    path,
                    cancel,
                )
            };
            export().map_err(|error| with_saved_preview(error, directory))?;
            timing("map_saved");
        }
        Some(manifest)
    } else {
        let world = generate_with_progress(true, |progress| {
            headless::generate_map_world(&project, &options, cancel, progress)
        })?;
        timing("generated");
        write_generated_map(mapper_studio::map_export::Input {
            project: &world.recipe, maps: &world.maps, solution: &world.solution,
            root: options.rust_root.as_deref().unwrap(), catalog: &world.catalog,
        }, map_output.as_deref().unwrap(), cancel)?;
        timing("map_saved");
        None
    };
    Ok(json!({"output":output,"manifest":manifest,"map":map_output}))
}

fn run(args: &[OsString], cancel: &AtomicBool) -> Result<Value, Error> {
    if args.first().and_then(|arg| arg.to_str()) == Some("generate") {
        papyrus_terrain::uniform_cliffs::without_forecast_replay(|| generate(args, cancel))
    } else {
        run_other(args, cancel)
    }
}

// Keep other command handlers out of the generation call stack.
#[inline(never)]
fn run_other(args: &[OsString], cancel: &AtomicBool) -> Result<Value, Error> {
    let command = args.first().map(|v| text(v)).transpose()?.unwrap_or("");
    match command {
        "content" => {
            match args.get(1).map(|v| text(v)).transpose()? {
                Some("list-custom-monuments") => {
                    let parsed = Args::parse(&args[2..], &[], &[])?;
                    parsed.count(0)?;
                    let entries = mapper_studio::rustedit_prefab::library_entries(|| cancel.load(Ordering::Relaxed))
                        .map_err(failure)?;
                    return Ok(json!(entries.into_iter()
                        .map(|entry| json!({"name": entry.name, "version": entry.version.as_str()}))
                        .collect::<Vec<_>>()));
                }
                Some("inspect") => {}
                _ => return Err(usage("Use mapper content inspect PREFAB or mapper content list-custom-monuments")),
            }
            let parsed = Args::parse(&args[2..], &[], &[])?;
            parsed.count(1)?;
            let source = mapper_studio::rustedit_prefab::Source::read(
                Path::new(&parsed.positionals[0]),
                || cancel.load(Ordering::Relaxed),
            ).map_err(failure)?;
            Ok(json!({
                "format": "rustedit-1-prefab",
                "prefabs": source.prefab.prefabs.len(),
                "terrainDescriptorBytes": source.prefab.terrain_descriptor.len(),
                "otherFields": source.prefab.other_fields,
                "terrainImages": source.terrain.iter().map(|image| json!({
                    "suffix": image.suffix,
                    "dimensions": image.dimensions,
                    "bytes": image.bytes.len(),
                })).collect::<Vec<_>>(),
            }))
        }
        "gui" => {
            let parsed = Args::parse(&args[1..], &[], &[])?;
            if parsed.positionals.len() > 1 {
                return Err(usage("gui accepts at most one project"));
            }
            if let Some(path) = parsed.positionals.first() {
                Project::load(Path::new(path)).map_err(failure)?;
            }
            let directory = std::env::current_exe()
                .map_err(failure)?
                .parent()
                .ok_or_else(|| failure("Missing application directory"))?
                .to_path_buf();
            let executable = directory.join(if cfg!(windows) {
                "mapper-studio.exe"
            } else {
                "mapper-studio"
            });
            let child = Command::new(executable)
                .args(&parsed.positionals)
                .spawn()
                .map_err(failure)?;
            Ok(json!({"launched":true,"pid":child.id()}))
        }
        "project" => {
            let sub = args
                .get(1)
                .map(|v| text(v))
                .transpose()?
                .ok_or_else(|| usage("Missing project command"))?;
            let parsed = Args::parse(
                &args[2..],
                if sub == "new" {
                    &[
                        "--seed",
                        "--size",
                        "--rust-root",
                        "--ocean",
                        "--force",
                        "--native-profile",
                    ]
                } else if sub == "bind-native-profile" {
                    &["--rust-root", "--profile", "--output", "--force"]
                } else if matches!(sub, "bind-gpu-erosion" | "use-cpu-erosion") {
                    &["--output", "--force"]
                } else if sub == "auto-erosion" {
                    &["--output", "--cpu-workers", "--gpu-memory-mib", "--force"]
                } else if sub == "bind-terrain-graph" {
                    &["--graph", "--output", "--force"]
                } else if sub == "clear-terrain-graph" {
                    &["--output", "--force"]
                } else if sub == "add-monument-addon" {
                    &["--source", "--anchor", "--output", "--force"]
                } else if sub == "add-custom-monument" {
                    &["--version", "--package", "--position", "--yaw", "--output", "--force"]
                } else if sub == "prepare-rivers" {
                    &["--rust-root", "--output", "--force"]
                } else if sub == "disable-rivers" {
                    &["--output", "--force"]
                } else if sub == "bind-roads" {
                    &[
                        "--rust-root",
                        "--catalog",
                        "--source",
                        "--surfaces",
                        "--staged-roadside",
                        "--ring-costmap-profile",
                        "--access-costmap-profile",
                        "--no-ring-costmap",
                        "--no-access-costmap",
                        "--constrained-road-smoothing",
                        "--no-constrained-road-smoothing",
                        "--main-ring",
                        "--no-main-ring",
                        "--trails",
                        "--no-trails",
                        "--ring-recovery",
                        "--no-ring-recovery",
                        "--connected-exits",
                        "--no-connected-exits",
                        "--road-topology",
                        "--no-road-topology",
                        "--no-staged-roads",
                        "--ordered-road-layout",
                        "--legacy-road-layout",
                        "--terrain-road-entrances",
                        "--no-terrain-road-entrances",
                        "--global-road-earthwork",
                        "--no-global-road-earthwork",
                        "--output",
                        "--force",
                    ]
                } else if sub == "prepare-rails" {
                    &[
                        "--rust-root",
                        "--catalog",
                        "--bindings",
                        "--settings",
                        "--output",
                        "--force",
                    ]
                } else if sub == "bind-rails" {
                    &[
                        "--rust-root",
                        "--catalog",
                        "--bindings",
                        "--source",
                        "--meshes",
                        "--settings",
                        "--output",
                        "--force",
                    ]
                } else if sub == "bind-objects" {
                    &[
                        "--rust-root",
                        "--catalog",
                        "--bindings",
                        "--road-source",
                        "--output",
                        "--force",
                    ]
                } else if sub == "bind-decor" {
                    &[
                        "--rust-root",
                        "--catalog",
                        "--source",
                        "--output",
                        "--force",
                    ]
                } else if sub == "inspect-object-workflow" {
                    &[
                        "--rust-root",
                        "--catalog",
                        "--infrastructure",
                        "--source",
                        "--decoration-source",
                    ]
                } else if sub == "inspect-road-objects" {
                    &[
                        "--rust-root",
                        "--catalog",
                        "--infrastructure",
                        "--source",
                        "--details",
                        "--terrain-bundle",
                        "--effects-output",
                        "--live-effects-output",
                    ]
                } else if sub == "inspect-rails" {
                    &[
                        "--rust-root",
                        "--catalog",
                        "--source",
                        "--infrastructure",
                        "--tracks",
                        "--meshes",
                        "--mesh-deform",
                        "--curve",
                        "--terrain-bundle",
                        "--route",
                        "--ring",
                        "--approaches",
                        "--bed",
                        "--bed-output",
                        "--cached",
                        "--tier-retries",
                        "--no-tier-retries",
                        "--authored-earthwork",
                        "--no-authored-earthwork",
                    ]
                } else if sub == "inspect-water" {
                    &["--rust-root", "--catalog", "--details", "--collision-cache"]
                } else if sub == "inspect-monuments" {
                    &[
                        "--rust-root",
                        "--catalog",
                        "--bindings",
                        "--source",
                        "--details",
                    ]
                } else if sub == "bind-monuments" {
                    &[
                        "--rust-root",
                        "--catalog",
                        "--bindings",
                        "--source",
                        "--output",
                        "--force",
                        "--tier-retries",
                        "--no-tier-retries",
                        "--authored-earthwork",
                        "--no-authored-earthwork",
                        "--stage-filters",
                        "--no-stage-filters",
                    ]
                } else if sub == "bind-assets" {
                    &[
                        "--rust-root",
                        "--catalog",
                        "--bindings",
                        "--output",
                        "--earthwork",
                        "--rebuild-water",
                        "--force",
                    ]
                } else {
                    &[]
                },
                &[
                    "--ocean",
                    "--force",
                    "--earthwork",
                    "--rebuild-water",
                    "--details",
                    "--cached",
                    "--main-ring",
                    "--no-main-ring",
                    "--trails",
                    "--no-trails",
                    "--ring-recovery",
                    "--no-ring-recovery",
                    "--connected-exits",
                    "--no-connected-exits",
                    "--road-topology",
                    "--no-road-topology",
                    "--no-staged-roads",
                    "--ordered-road-layout",
                    "--legacy-road-layout",
                    "--terrain-road-entrances",
                    "--no-terrain-road-entrances",
                    "--global-road-earthwork",
                    "--no-global-road-earthwork",
                    "--no-ring-costmap",
                    "--no-access-costmap",
                    "--constrained-road-smoothing",
                    "--no-constrained-road-smoothing",
                    "--stage-filters",
                    "--no-stage-filters",
                    "--tier-retries",
                    "--no-tier-retries",
                    "--authored-earthwork",
                    "--no-authored-earthwork",
                ],
            )?;
            parsed.count(1)?;
            if sub == "bind-roads" {
                if parsed.has("--ordered-road-layout") && parsed.has("--legacy-road-layout") {
                    return Err(usage(
                        "Choose --ordered-road-layout or --legacy-road-layout",
                    ));
                }
                if parsed.has("--ordered-road-layout") && parsed.has("--no-staged-roads") {
                    return Err(usage(
                        "--ordered-road-layout requires staged roads; remove --no-staged-roads",
                    ));
                }
                if parsed.has("--terrain-road-entrances")
                    && parsed.has("--no-terrain-road-entrances")
                {
                    return Err(usage(
                        "Choose --terrain-road-entrances or --no-terrain-road-entrances",
                    ));
                }
                if parsed.has("--terrain-road-entrances")
                    && (parsed.has("--legacy-road-layout") || parsed.has("--no-staged-roads"))
                {
                    return Err(usage(
                        "--terrain-road-entrances requires an ordered staged road layout",
                    ));
                }
                if parsed.has("--global-road-earthwork") && parsed.has("--no-global-road-earthwork")
                {
                    return Err(usage(
                        "Choose --global-road-earthwork or --no-global-road-earthwork",
                    ));
                }
                if parsed.has("--global-road-earthwork") && parsed.has("--no-staged-roads") {
                    return Err(usage(
                        "--global-road-earthwork requires staged roads; remove --no-staged-roads",
                    ));
                }
                if parsed.has("--global-road-earthwork") && parsed.has("--no-connected-exits") {
                    return Err(usage(
                        "--global-road-earthwork requires connected road exits; remove --no-connected-exits",
                    ));
                }
            }
            let path = Path::new(&parsed.positionals[0]);
            match sub {
                "add-custom-monument" => {
                    use mapper_studio::{custom_monuments, rustedit_prefab};
                    let required = |key: &str| {
                        parsed.flags.get(key).and_then(Option::as_ref)
                            .ok_or_else(|| usage(format!("add-custom-monument requires {key}")))
                            .and_then(|value| text(value))
                    };
                    let coordinates = required("--position")?.split(',').map(str::parse::<f32>)
                        .collect::<Result<Vec<_>, _>>().map_err(|_| usage("--position must be X,Y,Z in metres"))?;
                    let position = coordinates.try_into().map_err(|_| usage("--position must be X,Y,Z in metres"))?;
                    let yaw = match parsed.flags.get("--yaw").and_then(Option::as_ref) {
                        Some(value) => text(value)?.parse::<f32>().map_err(|_| usage("--yaw must be in degrees"))?,
                        None => 0.,
                    };
                    let output = parsed.path("--output").ok_or_else(|| usage("add-custom-monument requires --output"))?;
                    let mut project = Project::load(path).map_err(failure)?;
                    let cancelled = || cancel.load(Ordering::Relaxed);
                    let objects = match (parsed.path("--package"), parsed.has("--version")) {
                        (Some(file), false) => {
                            let package = mapper_studio::content_package::Imported::read_file(&file, cancelled).map_err(failure)?;
                            custom_monuments::add_package(&mut project, &package, position, yaw, cancelled).map_err(failure)?;
                            package.map.prefabs.len()
                        }
                        (None, true) => {
                            let version = rustedit_prefab::Version::parse(required("--version")?).map_err(failure)?;
                            let source = rustedit_prefab::read_version(&version, cancelled).map_err(failure)?;
                            custom_monuments::check_objects_only(&source).map_err(failure)?;
                            custom_monuments::add_placement(&mut project, custom_monuments::Placement {
                                version: version.as_str().to_owned(), position, yaw, baked: false,
                                name: String::new(), presence: mapper_studio::Presence::Required,
                            }).map_err(failure)?;
                            source.prefab.prefabs.len()
                        }
                        _ => return Err(usage("Choose either --version or --package for the custom monument")),
                    };
                    project.validate().map_err(failure)?;
                    save_project_edit(path, &output, &project, parsed.has("--force"), cancel)?;
                    let version = &project.custom_monuments.last().expect("placement was added").version;
                    Ok(json!({"project": output, "monument_version": version, "objects": objects}))
                }
                "add-monument-addon" => {
                    use mapper_studio::content_package::{Binding, Imported};

                    let mut project = Project::load(path).map_err(failure)?;
                    let source = parsed.path("--source")
                        .ok_or_else(|| usage("add-monument-addon requires --source"))?;
                    let output = parsed.path("--output")
                        .ok_or_else(|| usage("add-monument-addon requires --output"))?;
                    let item = Imported::read_file(&source, || cancel.load(Ordering::Relaxed))
                        .map_err(failure)?;
                    let anchor = match parsed.flags.get("--anchor").and_then(Option::as_ref) {
                        Some(index) => text(index)?.parse::<usize>()
                            .map_err(|_| usage("--anchor must be a zero-based prefab index"))?,
                        None => match &item.manifest.binding {
                            Binding::AddOn { anchor_index } => *anchor_index,
                            Binding::Standalone { .. } => {
                                return Err(usage("Choose the stock prefab with --anchor INDEX"));
                            }
                        },
                    };
                    let addon = mapper_studio::monument_addons::import(
                        &item, item.manifest.name.clone(), anchor,
                    ).map_err(failure)?;
                    let library = mapper_studio::monument_addons::Library::local().map_err(failure)?;
                    library.save_embedded(&project, addon.anchor_id).map_err(failure)?;
                    let version = library.store(&addon).map_err(failure)?;
                    let receipt = json!({
                        "project": output, "anchor_id": addon.anchor_id, "objects": addon.children.len(),
                        "overlay_version": version
                    });
                    project.choose_monument_overlay(addon.anchor_id, Some(version)).map_err(failure)?;
                    save_project_edit(path, &output, &project, parsed.has("--force"), cancel)?;
                    Ok(receipt)
                }
                "prepare-rivers" | "disable-rivers" => {
                    let mut project = Project::load(path).map_err(failure)?;
                    let output = parsed
                        .path("--output")
                        .ok_or_else(|| usage("River project commands require --output"))?;
                    if output.exists() && !parsed.has("--force") {
                        return Err(failure(
                            "Output already exists; choose another file or pass --force",
                        ));
                    }
                    let mut reused = None;
                    if sub == "prepare-rivers" {
                        let root = parsed
                            .path("--rust-root")
                            .ok_or_else(|| usage("prepare-rivers requires --rust-root"))?;
                        let installation = project.installation.as_ref().ok_or_else(|| {
                            failure("Bind an installed native terrain source first")
                        })?;
                        if !native_preview::is_native(&project.preview_engine) {
                            return Err(failure("Rivers require native terrain"));
                        }
                        let prepared = mapper_studio::river_extract::prepare(
                            &root,
                            installation,
                            project.rivers.as_ref(),
                            || cancel.load(Ordering::Relaxed),
                        )
                        .map_err(failure)?
                        .ok_or_else(|| Error {
                            code: 130,
                            message: "River preparation cancelled".into(),
                        })?;
                        reused = Some(prepared.reused);
                        let (catalog, _) = mapper_studio::asset_discovery::prepare(
                            &root, installation, project.monument_assets.as_ref(),
                            || cancel.load(Ordering::Relaxed),
                        ).map_err(failure)?;
                        let infrastructure = mapper_studio::road_extract::prepare(
                            &root, &catalog, project.roads.as_ref().map(|r| r.source_sha256.as_str()),
                            || cancel.load(Ordering::Relaxed),
                        ).map_err(failure)?;
                        let objects = mapper_studio::object_extract::prepare_rivers(
                            &root, &infrastructure,
                            project.rivers.as_ref().and_then(|r| r.objects.as_ref()).map(|r| r.source_sha256.as_str()),
                            || cancel.load(Ordering::Relaxed),
                        ).map_err(failure)?;
                        let mut recipe = prepared.source.recipe();
                        recipe.objects = Some(mapper_studio::river_objects::Recipe::bind(&objects, &infrastructure).map_err(failure)?);
                        project.rivers = Some(recipe);
                        if project.composition_engine.is_some()
                            && !papyrus_terrain::monument_composition::water_aware(
                                project.composition_engine.as_deref(),
                            )
                        {
                            project.composition_engine =
                                Some(papyrus_terrain::monument_composition::WATER_VERSION.into());
                        }
                    } else {
                        project.rivers = None;
                    }
                    mapper_studio::generation_defaults::apply(&mut project);
                    project.validate().map_err(failure)?;
                    save_project_edit(path, &output, &project, parsed.has("--force"), cancel)?;
                    Ok(
                        json!({"project":output,"rivers":project.rivers,"seed":project.seeds[2].value,"reused_source":reused}),
                    )
                }
                "bind-roads" => {
                    let mut project = Project::load(path).map_err(failure)?;
                    let root = parsed
                        .path("--rust-root")
                        .ok_or_else(|| usage("bind-roads requires --rust-root"))?;
                    let catalog = parsed
                        .path("--catalog")
                        .ok_or_else(|| usage("bind-roads requires --catalog"))?;
                    let source = parsed
                        .path("--source")
                        .ok_or_else(|| usage("bind-roads requires --source"))?;
                    let output = parsed
                        .path("--output")
                        .ok_or_else(|| usage("bind-roads requires --output"))?;
                    let expected = project
                        .installation
                        .as_ref()
                        .ok_or_else(|| failure("Bind a Rust installation first"))?;
                    let catalog =
                        mapper_studio::prefab_assets::load(&root, &catalog, expected, || {
                            cancel.load(Ordering::Relaxed)
                        })
                        .map_err(failure)?;
                    let pins = project
                        .monument_assets
                        .as_ref()
                        .ok_or_else(|| failure("Pin monument assets before enabling roads"))?;
                    project
                        .check_monument_assets(&catalog.sha256, &pins.bindings_sha256)
                        .map_err(failure)?;
                    let source =
                        mapper_studio::infrastructure::Source::cache(&source, &catalog, || {
                            cancel.load(Ordering::Relaxed)
                        })
                        .map_err(failure)?;
                    source
                        .verify_installation(&root, || cancel.load(Ordering::Relaxed))
                        .map_err(failure)?;
                    let selected = mapper_studio::infrastructure::Recipe::for_source(&source);
                    let mut roads = project.roads.take().unwrap_or_else(|| selected.clone());
                    if roads.source_sha256 != source.sha256 || roads.engine != selected.engine {
                        roads.surface_sha256 = None;
                        roads.staged = None;
                        roads.ring_costmap = None;
                    }
                    roads.source_sha256 = source.sha256.clone();
                    roads.engine = selected.engine;
                    if roads.engine != papyrus_terrain::road_network::COST_VERSION {
                        roads.main_ring = None;
                        roads.include_trails = None;
                        roads.ring_search = None;
                        roads.access_search = None;
                        roads.post_topology = None;
                    }
                    if parsed.has("--road-topology") && parsed.has("--no-road-topology") {
                        return Err(usage("Choose either --road-topology or --no-road-topology"));
                    }
                    if parsed.has("--road-topology") || parsed.has("--no-road-topology") {
                        if source.profile.post_topology.is_none()
                            || roads.engine != papyrus_terrain::road_network::COST_VERSION
                        {
                            return Err(usage(
                                "Road topology requires a captured post-road source stage",
                            ));
                        }
                        roads.post_topology = parsed
                            .has("--road-topology")
                            .then(|| papyrus_terrain::road_topology::VERSION.into());
                    }
                    if parsed.has("--connected-exits") && parsed.has("--no-connected-exits") {
                        return Err(usage(
                            "Choose either --connected-exits or --no-connected-exits",
                        ));
                    }
                    if parsed.has("--connected-exits") || parsed.has("--no-connected-exits") {
                        if roads.engine != papyrus_terrain::road_network::COST_VERSION {
                            return Err(usage(
                                "Connected exits require a source-cost road profile",
                            ));
                        }
                        roads.access_search = parsed
                            .has("--connected-exits")
                            .then(|| papyrus_terrain::road_network::COMPONENT_VERSION.into());
                    }
                    if parsed.has("--ring-recovery") && parsed.has("--no-ring-recovery") {
                        return Err(usage("Choose either --ring-recovery or --no-ring-recovery"));
                    }
                    if parsed.has("--ring-recovery") || parsed.has("--no-ring-recovery") {
                        if roads.engine != papyrus_terrain::road_network::COST_VERSION {
                            return Err(usage("Ring recovery requires a source-cost road profile"));
                        }
                        roads.ring_search = parsed
                            .has("--ring-recovery")
                            .then(|| papyrus_terrain::road_ring::RECOVERY_VERSION.into());
                    }
                    if parsed.has("--trails") && parsed.has("--no-trails") {
                        return Err(usage("Choose either --trails or --no-trails"));
                    }
                    if parsed.has("--trails") || parsed.has("--no-trails") {
                        if roads.engine != papyrus_terrain::road_network::COST_VERSION {
                            return Err(usage(
                                "Independent trail switches require a source-cost road profile",
                            ));
                        }
                        roads.include_trails = Some(parsed.has("--trails"));
                    }
                    if parsed.has("--main-ring") && parsed.has("--no-main-ring") {
                        return Err(usage("Choose either --main-ring or --no-main-ring"));
                    }
                    if parsed.has("--main-ring") {
                        if source.profile.ring.is_none() {
                            return Err(failure("This source has no captured main road ring"));
                        }
                        if roads.engine == papyrus_terrain::road_network::COST_VERSION {
                            roads.main_ring = Some(true);
                        } else {
                            roads.engine = papyrus_terrain::road_network::RING_VERSION.into();
                        }
                    } else if parsed.has("--no-main-ring") {
                        if roads.engine == papyrus_terrain::road_network::COST_VERSION {
                            roads.main_ring = Some(false);
                        } else if roads.engine == papyrus_terrain::road_network::RING_VERSION {
                            roads.engine = papyrus_terrain::road_network::SHAPED_VERSION.into();
                        }
                    }
                    if let Some(Some(digest)) = parsed.flags.get("--surfaces") {
                        if ![
                            papyrus_terrain::road_network::SHAPED_VERSION,
                            papyrus_terrain::road_network::RING_VERSION,
                            papyrus_terrain::road_network::COST_VERSION,
                        ]
                        .contains(&roads.engine.as_str())
                        {
                            return Err(failure("Road surfaces require a shaped road profile"));
                        }
                        let surfaces = mapper_studio::road_surface_source::Source::cached(
                            &root,
                            text(digest)?,
                            &source,
                            || cancel.load(Ordering::Relaxed),
                        )
                        .map_err(failure)?;
                        roads.surface_sha256 = Some(surfaces.sha256);
                    }
                    if parsed.has("--no-ring-costmap")
                        && parsed.path("--ring-costmap-profile").is_some()
                    {
                        return Err(usage("Choose --ring-costmap-profile or --no-ring-costmap"));
                    }
                    if parsed.has("--no-ring-costmap") {
                        roads.ring_costmap = None;
                    }
                    if let Some(path) = parsed.path("--ring-costmap-profile") {
                        let mut raw = Vec::new();
                        File::open(path)
                            .map_err(failure)?
                            .take(16385)
                            .read_to_end(&mut raw)
                            .map_err(failure)?;
                        if raw.len() > 16384 {
                            return Err(failure("Road cost profile exceeds 16 KiB"));
                        }
                        let profile: mapper_studio::road_cost_source::Profile =
                            serde_json::from_slice(&raw).map_err(failure)?;
                        profile.check(&source).map_err(failure)?;
                        roads.ring_costmap = Some(profile);
                    }
                    if parsed.has("--no-access-costmap")
                        && parsed.path("--access-costmap-profile").is_some()
                    {
                        return Err(usage(
                            "Choose --access-costmap-profile or --no-access-costmap",
                        ));
                    }
                    if parsed.has("--no-access-costmap") {
                        roads.access_costmap = None;
                        roads.access_river_water = None;
                        roads.access_smoothing = None;
                    }
                    if let Some(path) = parsed.path("--access-costmap-profile") {
                        let mut raw = Vec::new();
                        File::open(path)
                            .map_err(failure)?
                            .take(16385)
                            .read_to_end(&mut raw)
                            .map_err(failure)?;
                        if raw.len() > 16384 {
                            return Err(failure("Road cost profile exceeds 16 KiB"));
                        }
                        let profile: mapper_studio::road_cost_source::Profile =
                            serde_json::from_slice(&raw).map_err(failure)?;
                        profile.check(&source).map_err(failure)?;
                        roads.access_river_water =
                            profile.joins.as_ref().and_then(|j| j.river_water.clone());
                        roads.access_costmap = Some(profile);
                        roads.access_smoothing = Some(papyrus_terrain::road_smoothing::VERSION.into());
                    }
                    if parsed.has("--no-constrained-road-smoothing") {
                        return Err(usage("Road smoothing is required and cannot be disabled"));
                    }
                    if parsed.has("--staged-roadside") && parsed.has("--no-staged-roads") {
                        return Err(usage("Choose --staged-roadside or --no-staged-roads"));
                    }
                    if parsed.has("--no-staged-roads") {
                        roads.set_global_earthwork(false).map_err(failure)?;
                        roads.staged = None;
                    }
                    if let Some(Some(digest)) = parsed.flags.get("--staged-roadside") {
                        if project.rails.is_some() {
                            return Err(failure(
                                "Staged roads need retained rail-height support before enabling rails",
                            ));
                        }
                        let roadside = mapper_studio::roadside_source::Source::cached(
                            text(digest)?,
                            &root,
                            &catalog,
                            || cancel.load(Ordering::Relaxed),
                        )
                        .map_err(failure)?;
                        let paint = mapper_studio::road_paint_source::Profile::for_source(&roadside)
                            .map_err(failure)?
                            .ok_or_else(|| failure("No matching staged road paint profile"))?;
                        mapper_studio::road_terrain_source::Profile::for_paint(&paint)
                            .map_err(failure)?
                            .ok_or_else(|| failure("No matching staged road terrain profile"))?;
                        let roadside_preferences = roads.staged.take()
                            .map_or_else(Vec::new, |policy| policy.roadside_preferences);
                        roads.staged = Some(mapper_studio::road_generation::Policy {
                            layout: None,
                            engine: mapper_studio::road_generation::VERSION.into(),
                            roadside_source_sha256: roadside.sha256,
                            roadside_preferences,
                        });
                    }
                    if parsed.has("--ordered-road-layout") {
                        let staged = roads
                            .staged
                            .as_mut()
                            .ok_or_else(|| failure("Ordered layout requires staged roads"))?;
                        let inputs = mapper_studio::road_extract::prepare_ordered(&root, &catalog, &source,
                            None, || cancel.load(Ordering::Relaxed)).map_err(failure)?
                            .ok_or_else(|| failure("Ordered layout preparation cancelled or unsupported for this Rust version"))?;
                        staged.layout =
                            Some(mapper_studio::source_road_planning::Policy::bind(&inputs));
                    } else if parsed.has("--legacy-road-layout")
                        && let Some(staged) = &mut roads.staged
                    {
                        staged.layout = None;
                    }
                    if parsed.has("--terrain-road-entrances")
                        || parsed.has("--no-terrain-road-entrances")
                    {
                        let layout = roads.staged.as_mut().and_then(|s| s.layout.as_mut());
                        if let Some(layout) = layout {
                            layout.terrain_entrances =
                                parsed.has("--terrain-road-entrances").then(|| {
                                    papyrus_terrain::source_road_route::TERRAIN_ENTRANCE_ROUTED_VERSION
                                        .into()
                                });
                        } else if parsed.has("--terrain-road-entrances") {
                            return Err(usage(
                                "--terrain-road-entrances requires --ordered-road-layout or an existing ordered layout",
                            ));
                        }
                    }
                    // Native sampling requires the complete ordered layout before
                    // the earthwork setter validates the resulting road recipe.
                    if parsed.has("--global-road-earthwork")
                        || parsed.has("--no-global-road-earthwork")
                    {
                        roads
                            .set_global_earthwork(parsed.has("--global-road-earthwork"))
                            .map_err(failure)?;
                    }
                    roads.apply_source_defaults(&source).map_err(failure)?;
                    project.roads = Some(roads);
                    mapper_studio::generation_defaults::apply(&mut project);
                    project.validate().map_err(failure)?;
                    if cancel.load(Ordering::Relaxed) {
                        return Err(failure("Road binding cancelled"));
                    }
                    save_project_edit(path, &output, &project, parsed.has("--force"), cancel)?;
                    Ok(
                        json!({"project":output,"road_source_sha256":source.sha256,"profile":source.profile.id,"prefab_count":source.prefabs.len(),"road_generation":project.roads.as_ref().map(|r|r.engine.as_str()),"scope":"preview corridors; full map assembly remains incomplete"}),
                    )
                }
                "inspect-road-objects" | "inspect-object-workflow" => {
                    if parsed.path("--effects-output").is_some()
                        && parsed.path("--live-effects-output").is_some()
                    {
                        return Err(usage(
                            "Choose either --effects-output or --live-effects-output",
                        ));
                    }
                    if (parsed.path("--effects-output").is_some()
                        || parsed.path("--live-effects-output").is_some())
                        && parsed.path("--terrain-bundle").is_none()
                    {
                        return Err(usage("--effects-output requires --terrain-bundle"));
                    }
                    let project = Project::load(path).map_err(failure)?;
                    let root = parsed
                        .path("--rust-root")
                        .ok_or_else(|| usage("inspect-road-objects requires --rust-root"))?;
                    let catalog_path = parsed
                        .path("--catalog")
                        .ok_or_else(|| usage("inspect-road-objects requires --catalog"))?;
                    let infra_path = parsed
                        .path("--infrastructure")
                        .ok_or_else(|| usage("inspect-road-objects requires --infrastructure"))?;
                    let source_path = parsed
                        .path("--source")
                        .ok_or_else(|| usage("inspect-road-objects requires --source"))?;
                    let expected = project
                        .installation
                        .as_ref()
                        .ok_or_else(|| failure("Bind a Rust installation first"))?;
                    let catalog =
                        mapper_studio::prefab_assets::load(&root, &catalog_path, expected, || {
                            cancel.load(Ordering::Relaxed)
                        })
                        .map_err(failure)?;
                    let parent =
                        mapper_studio::infrastructure::Source::load(&infra_path, &catalog, || {
                            cancel.load(Ordering::Relaxed)
                        })
                        .map_err(failure)?;
                    let source = mapper_studio::road_object_source::Source::load(
                        &root,
                        &source_path,
                        &parent,
                        || cancel.load(Ordering::Relaxed),
                    )
                    .map_err(failure)?;
                    if sub == "inspect-object-workflow" {
                        let path = parsed.path("--decoration-source").ok_or_else(|| {
                            usage("inspect-object-workflow requires --decoration-source")
                        })?;
                        let decoration =
                            mapper_studio::decor_source::Source::load(&root, &path, &catalog, || {
                                cancel.load(Ordering::Relaxed)
                            })
                            .map_err(failure)?;
                        let plan = mapper_studio::object_workflow::plan(&source, &decoration.stages)
                            .map_err(failure)?;
                        let mut report = serde_json::to_value(plan).map_err(failure)?;
                        report["decoration_source_sha256"] = json!(decoration.sha256);
                        return Ok(report);
                    }
                    let mut report = source.inspect(parsed.has("--details"));
                    if let Some(bundle) = parsed.path("--terrain-bundle") {
                        if project
                            .roads
                            .as_ref()
                            .is_none_or(|r| r.source_sha256 != source.infrastructure_sha256)
                        {
                            return Err(failure(
                                "Road object source differs from the saved road source",
                            ));
                        }
                        let terrain =
                            mapper_studio::rail_preview::Terrain::load(&bundle, &project, || {
                                cancel.load(Ordering::Relaxed)
                            })
                            .map_err(failure)?;
                        let (layers, alpha) = terrain
                            .bed_layers(&bundle, || cancel.load(Ordering::Relaxed))
                            .map_err(failure)?;
                        let road_report = terrain
                            .road_report(&bundle, || cancel.load(Ordering::Relaxed))
                            .map_err(failure)?;
                        let roads = mapper_studio::road_object_placement::roads_from_report(
                            &road_report,
                            parent
                                .profile
                                .road_hierarchy
                                .ok_or_else(|| failure("Missing source road hierarchy"))?,
                            parent.profile.width,
                        )
                        .map_err(failure)?;
                        let settings = mapper_studio::road_object_placement::Settings {
                            seed: project.seeds[3].value,
                            maximum_work: 2_000_000,
                            maximum_poses: 100_000,
                        };
                        let maximum_cut_fill = project
                            .roads
                            .as_ref()
                            .ok_or_else(|| failure("Missing road settings"))?
                            .maximum_cut_fill;
                        let (fitting, live_output) =
                            if parsed.path("--live-effects-output").is_some() {
                                let generated = mapper_studio::road_object_effects::generate(
                                    &layers,
                                    &alpha,
                                    &roads,
                                    &source,
                                    &parent,
                                    settings,
                                    maximum_cut_fill,
                                    || cancel.load(Ordering::Relaxed),
                                )
                                .map_err(failure)?
                                .ok_or_else(|| failure("Live road-object generation cancelled"))?;
                                (generated.fitting, Some(generated.terrain))
                            } else {
                                let fitting = mapper_studio::road_object_placement::fit(
                                    &layers,
                                    &alpha,
                                    &roads,
                                    &source,
                                    settings,
                                    || cancel.load(Ordering::Relaxed),
                                )
                                .map_err(failure)?
                                .ok_or_else(|| failure("Road object fitting cancelled"))?;
                                (fitting, None)
                            };
                        if let Some(directory) = parsed
                            .path("--effects-output")
                            .or_else(|| parsed.path("--live-effects-output"))
                        {
                            let output = if let Some(output) = live_output {
                                output
                            } else {
                                mapper_studio::road_object_effects::apply(
                                    &layers,
                                    &alpha,
                                    &source,
                                    &parent,
                                    &fitting,
                                    maximum_cut_fill,
                                    || cancel.load(Ordering::Relaxed),
                                )
                                .map_err(failure)?
                                .ok_or_else(|| failure("Road object effects cancelled"))?
                            };
                            let summary = serde_json::to_value(&output.report).map_err(failure)?;
                            let manifest = mapper_studio::road_object_effects::publish(
                                output,
                                mapper_studio::road_object_effects::Preview {
                                    directory: &directory,
                                    input_directory: &bundle,
                                    input_manifest_sha256: terrain.manifest_digest(),
                                    base: &layers,
                                    alpha: &alpha,
                                    fitting: &fitting,
                                },
                                || cancel.load(Ordering::Relaxed),
                            )
                            .map_err(failure)?;
                            report["terrain_effects"] = summary;
                            report["terrain_preview"] =
                                json!({"directory":directory,"manifest":manifest});
                        }
                        report["fitting"] = serde_json::to_value(fitting).map_err(failure)?;
                        report["terrain_manifest_sha256"] = json!(terrain.manifest_digest());
                        report["pending_checks"] = json!([
                            "Generation-pipeline integration and road-surface refresh after terrain effects",
                            "Any unsupported source sequence adapters",
                            "Prefab assembly, cables, collisions and in-game validation"
                        ]);
                    }
                    Ok(report)
                }
                "bind-rails" | "prepare-rails" => {
                    let mut project = Project::load(path).map_err(failure)?;
                    let required = |flag| {
                        parsed
                            .path(flag)
                            .ok_or_else(|| usage(format!("{sub} requires {flag}")))
                    };
                    let root = required("--rust-root")?;
                    let catalog_path = required("--catalog")?;
                    let bindings_path = required("--bindings")?;
                    let settings_path = required("--settings")?;
                    let output = required("--output")?;
                    if output.exists() && !parsed.has("--force") {
                        return Err(failure(
                            "Output already exists; choose another file or pass --force",
                        ));
                    }

                    let read = |p: &Path, limit: u64| -> Result<Vec<u8>, Error> {
                        let mut raw = Vec::new();
                        File::open(p)
                            .map_err(failure)?
                            .take(limit + 1)
                            .read_to_end(&mut raw)
                            .map_err(failure)?;
                        if raw.len() as u64 > limit {
                            return Err(failure("Rail binding input exceeds its size limit"));
                        }
                        Ok(raw)
                    };
                    let settings: Value =
                        serde_json::from_slice(&read(&settings_path, 16384)?).map_err(failure)?;
                    // Validate settings before verifying large game asset bundles.
                    mapper_studio::rail_generation::Recipe {
                        engine: mapper_studio::rail_generation::VERSION.into(),
                        source_sha256: "0".repeat(64),
                        mesh_sha256: "0".repeat(64),
                        settings: settings.clone(),
                    }
                    .validate()
                    .map_err(failure)?;
                    if sub == "prepare-rails"
                        && (project.monument_rules.is_none() || project.roads.is_none())
                    {
                        return Err(failure(
                            "Prepare and enable monument placement and roads before rails",
                        ));
                    }
                    let catalog = mapper_studio::prefab_assets::load(
                        &root,
                        &catalog_path,
                        project
                            .installation
                            .as_ref()
                            .ok_or_else(|| failure("Bind a Rust installation first"))?,
                        || cancel.load(Ordering::Relaxed),
                    )
                    .map_err(failure)?;
                    let bindings = mapper_studio::prefab_textures::Bindings::load(
                        &root,
                        &bindings_path,
                        &catalog,
                        || cancel.load(Ordering::Relaxed),
                    )
                    .map_err(failure)?;
                    let (source_sha256, mesh_sha256) = if sub == "prepare-rails" {
                        let parent = mapper_studio::infrastructure::Source::cached(
                            &catalog,
                            project
                                .roads
                                .as_ref()
                                .ok_or_else(|| failure("Prepare and enable roads before rails"))?,
                            || cancel.load(Ordering::Relaxed),
                        )
                        .map_err(failure)?;
                        eprintln!(
                            "Preparing installation rail metadata and templates; Ctrl+C cancels."
                        );
                        let prepared = mapper_studio::rail_extract::prepare(
                            &root,
                            &catalog,
                            &catalog_path,
                            &parent,
                            project.rails.as_ref(),
                            || cancel.load(Ordering::Relaxed),
                        )
                        .map_err(failure)?;
                        (prepared.source.sha256, prepared.meshes.sha256)
                    } else {
                        let source_path = required("--source")?;
                        let meshes_path = required("--meshes")?;
                        (
                            format!("{:x}", Sha256::digest(read(&source_path, 4 * 1024 * 1024)?)),
                            format!(
                                "{:x}",
                                Sha256::digest(read(&meshes_path, 32 * 1024 * 1024)?)
                            ),
                        )
                    };
                    let recipe = mapper_studio::rail_generation::bind(
                        &project,
                        &catalog,
                        &bindings,
                        source_sha256,
                        mesh_sha256,
                        settings,
                        || cancel.load(Ordering::Relaxed),
                    )
                    .map_err(failure)?;
                    project.rails = Some(recipe);
                    mapper_studio::generation_defaults::apply(&mut project);
                    project.validate().map_err(failure)?;
                    if cancel.load(Ordering::Relaxed) {
                        return Err(failure("Rail binding cancelled"));
                    }
                    save_project_edit(path, &output, &project, parsed.has("--force"), cancel)?;
                    Ok(
                        json!({"project":output,"rails":project.rails,"scope":"normal surface rail preview; runtime connections, clearance and full world acceptance pending"}),
                    )
                }
                "bind-objects" => {
                    let mut project = Project::load(path).map_err(failure)?;
                    let required = |flag| {
                        parsed
                            .path(flag)
                            .ok_or_else(|| usage(format!("bind-objects requires {flag}")))
                    };
                    let root = required("--rust-root")?;
                    let catalog_path = required("--catalog")?;
                    let bindings_path = required("--bindings")?;
                    let output = required("--output")?;
                    let digest = parsed
                        .flags
                        .get("--road-source")
                        .and_then(|v| v.as_ref())
                        .ok_or_else(|| usage("bind-objects requires --road-source"))?;
                    let catalog = mapper_studio::prefab_assets::load(
                        &root,
                        &catalog_path,
                        project
                            .installation
                            .as_ref()
                            .ok_or_else(|| failure("Bind a Rust installation first"))?,
                        || cancel.load(Ordering::Relaxed),
                    )
                    .map_err(failure)?;
                    let bindings = mapper_studio::prefab_textures::Bindings::load(
                        &root,
                        &bindings_path,
                        &catalog,
                        || cancel.load(Ordering::Relaxed),
                    )
                    .map_err(failure)?;
                    let recipe = mapper_studio::object_generation::bind(
                        &project,
                        &catalog,
                        &bindings,
                        text(digest)?.to_owned(),
                        || cancel.load(Ordering::Relaxed),
                    )
                    .map_err(failure)?;
                    project.objects = Some(recipe);
                    mapper_studio::generation_defaults::apply(&mut project);
                    project.validate().map_err(failure)?;
                    if cancel.load(Ordering::Relaxed) {
                        return Err(failure("Live object binding cancelled"));
                    }
                    save_project_edit(path, &output, &project, parsed.has("--force"), cancel)?;
                    Ok(
                        json!({"project":output,"objects":project.objects,"scope":"source-ordered road and static object handlers; remaining world phases and assembly pending"}),
                    )
                }
                "bind-decor" => {
                    let mut project = Project::load(path).map_err(failure)?;
                    let root = parsed
                        .path("--rust-root")
                        .ok_or_else(|| usage("bind-decor requires --rust-root"))?;
                    let catalog_path = parsed
                        .path("--catalog")
                        .ok_or_else(|| usage("bind-decor requires --catalog"))?;
                    let source_path = parsed
                        .path("--source")
                        .ok_or_else(|| usage("bind-decor requires --source"))?;
                    let output = parsed
                        .path("--output")
                        .ok_or_else(|| usage("bind-decor requires --output"))?;
                    let expected = project
                        .installation
                        .as_ref()
                        .ok_or_else(|| failure("Bind a Rust installation first"))?;
                    let catalog =
                        mapper_studio::prefab_assets::load(&root, &catalog_path, expected, || {
                            cancel.load(Ordering::Relaxed)
                        })
                        .map_err(failure)?;
                    let pins = project
                        .monument_assets
                        .as_ref()
                        .ok_or_else(|| failure("Pin monument assets before enabling decoration"))?;
                    project
                        .check_monument_assets(&catalog.sha256, &pins.bindings_sha256)
                        .map_err(failure)?;
                    let source = mapper_studio::decor_source::Source::cache(
                        &root,
                        &source_path,
                        &catalog,
                        || cancel.load(Ordering::Relaxed),
                    )
                    .map_err(failure)?;
                    project.decoration = Some(source.recipe());
                    mapper_studio::generation_defaults::apply(&mut project);
                    project.validate().map_err(failure)?;
                    if cancel.load(Ordering::Relaxed) {
                        return Err(failure("Decoration binding cancelled"));
                    }
                    save_project_edit(path, &output, &project, parsed.has("--force"), cancel)?;
                    Ok(
                        json!({"project":output,"decor_source_sha256":source.sha256,"stages":source.stages.len(),"decoration_engine":project.decoration.as_ref().map(|r|&r.engine),"decoration":"provisional static geometry; terrain effects and runtime validation pending; runtime populations excluded"}),
                    )
                }
                "bind-gpu-erosion" | "use-cpu-erosion" => {
                    let mut project = Project::load(path).map_err(failure)?;
                    let output = parsed.path("--output")
                        .ok_or_else(|| usage("Erosion selection requires --output"))?;
                    project.gpu_erosion = if sub == "bind-gpu-erosion" {
                        if !project.supports_gpu_erosion() {
                            return Err(failure("GPU erosion requires an installation-backed source composition recipe"));
                        }
                        Some(mapper_studio::erosion_compute::detect().map_err(failure)?)
                    } else {
                        None
                    };
                    project.validate().map_err(failure)?;
                    save_project_edit(path, &output, &project, parsed.has("--force"), cancel)?;
                    Ok(json!({"project":output,"gpu_erosion":project.gpu_erosion}))
                }
                "auto-erosion" => {
                    let mut project = Project::load(path).map_err(failure)?;
                    if !project.supports_gpu_erosion() {
                        return Err(failure("Automatic erosion selection requires an installation-backed source composition recipe"));
                    }
                    let output = parsed.path("--output")
                        .ok_or_else(|| usage("auto-erosion requires --output"))?;
                    let force = parsed.has("--force");
                    if output.exists() && !force {
                        return Err(failure("Output already exists; choose another file or pass --force"));
                    }
                    mapper_studio::generation_resources::configure(compute_limits(&parsed)?)
                        .map_err(failure)?;
                    eprintln!("Checking erosion compute");
                    let report = mapper_studio::erosion_compute::recommend(
                        project.size, || cancel.load(Ordering::Relaxed),
                    ).map_err(failure)?
                        .ok_or_else(|| failure("Erosion selection cancelled"))?;
                    if cancel.load(Ordering::Relaxed) {
                        return Err(failure("Erosion selection cancelled"));
                    }
                    project.gpu_erosion = report.gpu.clone().filter(|_| report.prefer_gpu);
                    project.validate().map_err(failure)?;
                    save_project_edit(path, &output, &project, force, cancel)?;
                    Ok(json!({"project":output,"gpu_erosion":project.gpu_erosion,"comparison":report}))
                }
                "bind-terrain-graph" | "clear-terrain-graph" => {
                    let mut project = Project::load(path).map_err(failure)?;
                    let output = parsed.path("--output")
                        .ok_or_else(|| usage("Terrain graph editing requires --output"))?;
                    project.terrain_graph = if sub == "bind-terrain-graph" {
                        let path = parsed.path("--graph")
                            .ok_or_else(|| usage("bind-terrain-graph requires --graph"))?;
                        Some(Arc::new(papyrus_terrain::terrain_graph::Graph::read(
                            File::open(path).map_err(failure)?,
                        ).map_err(failure)?))
                    } else {
                        None
                    };
                    project.validate().map_err(failure)?;
                    if cancel.load(Ordering::Relaxed) {
                        return Err(failure("Terrain graph editing cancelled"));
                    }
                    save_project_edit(path, &output, &project, parsed.has("--force"), cancel)?;
                    Ok(json!({"project":output,"terrain_graph":project.terrain_graph}))
                }
                "bind-native-profile" => {
                    let mut project = Project::load(path).map_err(failure)?;
                    let root = parsed
                        .path("--rust-root")
                        .ok_or_else(|| usage("bind-native-profile requires --rust-root"))?;
                    let selected = parsed
                        .path("--profile")
                        .ok_or_else(|| usage("bind-native-profile requires --profile"))?;
                    let output = parsed
                        .path("--output")
                        .ok_or_else(|| usage("bind-native-profile requires --output"))?;
                    let actual = installation::inspect(&root, || cancel.load(Ordering::Relaxed))
                        .map_err(failure)?;
                    if project.installation.as_ref() != Some(&actual) {
                        return Err(failure(
                            "Rust installation does not match the saved recipe source",
                        ));
                    }
                    let selected = native_profile::Profile::load(&selected).map_err(failure)?;
                    selected.matches(&actual).map_err(failure)?;
                    let digest = selected.sha256().map_err(failure)?;
                    project.native_profile = Some(selected);
                    mapper_studio::generation_defaults::apply(&mut project);
                    project.validate().map_err(failure)?;
                    if cancel.load(Ordering::Relaxed) {
                        return Err(failure("Native profile binding cancelled"));
                    }
                    save_project_edit(path, &output, &project, parsed.has("--force"), cancel)?;
                    Ok(
                        json!({"project":output,"native_profile_sha256":digest,"terrain_engine":project.preview_engine}),
                    )
                }
                "inspect-rails" => {
                    if parsed.has("--mesh-deform")
                        && (!parsed.has("--meshes") || !parsed.has("--curve"))
                    {
                        return Err(usage("--mesh-deform requires --meshes and --curve"));
                    }
                    if parsed.has("--bed-output") && !parsed.has("--bed") {
                        return Err(usage("--bed-output requires --bed"));
                    }
                    if parsed.has("--bed") && !parsed.has("--approaches") {
                        return Err(usage(
                            "--bed requires --approaches, --ring and --terrain-bundle",
                        ));
                    }
                    if parsed.path("--bed-output").is_some_and(|p| p.exists()) {
                        return Err(failure("Choose a new bed preview output directory"));
                    }
                    if parsed.has("--approaches") && !parsed.has("--ring") {
                        return Err(usage("--approaches requires --ring and --terrain-bundle"));
                    }
                    if (parsed.has("--route") || parsed.has("--ring"))
                        && !parsed.has("--terrain-bundle")
                    {
                        return Err(usage("--route and --ring require --terrain-bundle"));
                    }
                    let project = Project::load(path).map_err(failure)?;
                    let root = parsed
                        .path("--rust-root")
                        .ok_or_else(|| usage("inspect-rails requires --rust-root"))?;
                    let catalog_path = parsed
                        .path("--catalog")
                        .ok_or_else(|| usage("inspect-rails requires --catalog"))?;
                    let source_path = parsed
                        .path("--source")
                        .ok_or_else(|| usage("inspect-rails requires --source"))?;
                    let cancelled = || cancel.load(Ordering::Relaxed);
                    let catalog = mapper_studio::prefab_assets::load(
                        &root,
                        &catalog_path,
                        project
                            .installation
                            .as_ref()
                            .ok_or_else(|| failure("Recipe has no saved installation identity"))?,
                        cancelled,
                    )
                    .map_err(failure)?;
                    let infrastructure = if let Some(path) = parsed.path("--infrastructure") {
                        mapper_studio::infrastructure::Source::load(&path, &catalog, cancelled)
                    } else if let Some(recipe) = &project.roads {
                        mapper_studio::infrastructure::Source::cached(&catalog, recipe, cancelled)
                    } else {
                        mapper_studio::infrastructure::Source::discover(&catalog, None, cancelled)
                    }
                    .map_err(failure)?;
                    let source = mapper_studio::rail_source::Source::load(
                        &root,
                        &source_path,
                        &catalog,
                        &infrastructure,
                        cancelled,
                    )
                    .map_err(failure)?;
                    let mut result = source
                        .audit(&infrastructure, project.size, parsed.has("--cached"), true)
                        .map_err(failure)?;
                    let authored_tracks = if let Some(path) = parsed.path("--tracks") {
                        Some(
                            mapper_studio::authored_tracks::Source::load(
                                &root,
                                &path,
                                &catalog,
                                &infrastructure,
                                &source,
                                cancelled,
                            )
                            .map_err(failure)?,
                        )
                    } else {
                        None
                    };
                    let mesh_source = if let Some(path) = parsed.path("--meshes") {
                        let meshes = mapper_studio::rail_mesh_source::Source::load(
                            &root, &path, &source, cancelled,
                        )
                        .map_err(failure)?;
                        result["mesh_templates"] = meshes.audit();
                        Some(meshes)
                    } else {
                        None
                    };
                    if let Some(tracks) = &authored_tracks {
                        result["authored_tracks"] =
                            tracks.audit(&infrastructure, &source).map_err(failure)?;
                    }
                    if let Some(bundle) = parsed.path("--terrain-bundle") {
                        let terrain =
                            mapper_studio::rail_preview::Terrain::load(&bundle, &project, cancelled)
                                .map_err(failure)?;
                        let grid = terrain
                            .grid(&source.profile.costmap, cancelled)
                            .map_err(failure)?
                            .ok_or_else(|| failure("Rail terrain planning cancelled"))?;
                        let query = if let Some(path) = parsed.path("--route") {
                            let mut raw = Vec::new();
                            File::open(path)
                                .map_err(failure)?
                                .take(16385)
                                .read_to_end(&mut raw)
                                .map_err(failure)?;
                            if raw.len() > 16384 {
                                return Err(failure("Rail route query exceeds 16 KiB"));
                            }
                            Some(
                                serde_json::from_slice::<mapper_studio::rail_preview::Query>(&raw)
                                    .map_err(failure)?,
                            )
                        } else {
                            None
                        };
                        result["terrain_plan"] = terrain
                            .audit(&grid, query.as_ref(), cancelled)
                            .map_err(failure)?;
                        if let Some(path) = parsed.path("--ring") {
                            let mut raw = Vec::new();
                            File::open(path)
                                .map_err(failure)?
                                .take(16385)
                                .read_to_end(&mut raw)
                                .map_err(failure)?;
                            if raw.len() > 16384 {
                                return Err(failure("Ring query exceeds 16 KiB"));
                            }
                            let settings: papyrus_terrain::rail_ring::Settings =
                                serde_json::from_slice(&raw).map_err(failure)?;
                            settings.validate().map_err(failure)?;
                            let policy=source.profile.ring.as_ref().ok_or_else(||failure("This rail snapshot has no source-bound ring adapter; prepare a new snapshot"))?;
                            let sockets = terrain
                                .sockets(
                                    &bundle,
                                    &project,
                                    &infrastructure,
                                    source.profile.rail_type,
                                    cancelled,
                                )
                                .map_err(failure)?;
                            let approach_settings = if let Some(path) = parsed.path("--approaches")
                            {
                                if source.profile.approaches.is_none() {
                                    return Err(failure(
                                        "This rail snapshot has no source-bound approach adapter; prepare a new snapshot",
                                    ));
                                }
                                let mut raw = Vec::new();
                                File::open(path)
                                    .map_err(failure)?
                                    .take(16385)
                                    .read_to_end(&mut raw)
                                    .map_err(failure)?;
                                if raw.len() > 16384 {
                                    return Err(failure("Approach query exceeds 16 KiB"));
                                }
                                let settings: papyrus_terrain::rail_approaches::Settings =
                                    serde_json::from_slice(&raw).map_err(failure)?;
                                settings.validate().map_err(failure)?;
                                Some(settings)
                            } else {
                                None
                            };
                            let layout_stage = source
                                .stages
                                .iter()
                                .find(|s| {
                                    s.kind == mapper_studio::rail_source::Kind::GenerateRailLayout
                                })
                                .ok_or_else(|| failure("Rail source lacks a layout stage"))?;
                            let approach_skip = source.skip(
                                layout_stage,
                                project.size,
                                parsed.has("--cached"),
                                true,
                            );
                            if approach_settings.is_some() {
                                result["terrain_plan"]["approaches"] = json!({"result":{
                                    "status":if approach_skip.is_some(){"skipped"}else{"unresolved"},
                                    "reason":approach_skip.unwrap_or("No primary ring candidate")},
                                    "terrain_applied":false,"socket_connections_validated":false});
                            }
                            let stage = source
                                .stages
                                .iter()
                                .find(|s| s.kind == mapper_studio::rail_source::Kind::GenerateRailRing)
                                .ok_or_else(|| failure("Rail source lacks a ring stage"))?;
                            let ring = if let Some(reason) =
                                source.skip(stage, project.size, parsed.has("--cached"), true)
                            {
                                json!({"status":"skipped","reason":reason})
                            } else {
                                let positions = sockets
                                    .sockets
                                    .iter()
                                    .map(|s| [s.position[0], s.position[2]])
                                    .collect::<Vec<_>>();
                                let ring = papyrus_terrain::rail_ring::build(
                                    &grid, policy, settings, &positions, cancelled,
                                )
                                .map_err(failure)?;
                                if matches!(ring, papyrus_terrain::rail_ring::Outcome::Cancelled) {
                                    return Err(failure("Rail ring planning cancelled"));
                                }
                                if let Some(settings) = approach_settings
                                    && approach_skip.is_none()
                                {
                                    result["terrain_plan"]["approaches"] =
                                        mapper_studio::rail_preview::approach_report(
                                            &grid, &source, &sockets, &ring, settings, cancelled,
                                        )
                                        .map_err(failure)?;
                                }
                                serde_json::to_value(ring).map_err(failure)?
                            };
                            result["terrain_plan"]["ring"] = json!({"engine":papyrus_terrain::rail_ring::VERSION,
                                "settings":settings,"policy":policy,"result":ring,"connector_protection":sockets,
                                "terrain_applied":false,"curve_validated":false,"socket_connections_validated":false});
                        }
                        if let Some(path) = parsed.path("--bed") {
                            let mut raw = Vec::new();
                            File::open(path)
                                .map_err(failure)?
                                .take(16385)
                                .read_to_end(&mut raw)
                                .map_err(failure)?;
                            if raw.len() > 16384 {
                                return Err(failure("Bed query exceeds 16 KiB"));
                            }
                            let settings: mapper_studio::rail_bed_preview::Query =
                                serde_json::from_slice(&raw).map_err(failure)?;
                            settings.validate().map_err(failure)?;
                            for kind in [
                                mapper_studio::rail_source::Kind::GenerateRailTerrain,
                                mapper_studio::rail_source::Kind::GenerateRailTexture,
                                mapper_studio::rail_source::Kind::GenerateRailTopology,
                            ] {
                                let stage = source
                                    .stages
                                    .iter()
                                    .find(|s| s.kind == kind)
                                    .ok_or_else(|| failure("Missing required bed stage"))?;
                                if let Some(reason) =
                                    source.skip(stage, project.size, parsed.has("--cached"), true)
                                {
                                    return Err(failure(format!(
                                        "Rail bed stage unavailable: {reason}"
                                    )));
                                }
                            }
                            let candidate = mapper_studio::rail_bed_preview::build(
                                &terrain,
                                &bundle,
                                &source,
                                &infrastructure,
                                &result,
                                settings,
                                authored_tracks.as_ref(),
                                mesh_source.as_ref(),
                                cancelled,
                            )
                            .map_err(failure)?
                            .ok_or_else(|| failure("Rail bed preparation cancelled"))?;
                            result["terrain_plan"]["bed"] = candidate.report.clone();
                            if let Some(output) = parsed.path("--bed-output") {
                                let manifest = mapper_studio::rail_bed_preview::publish(
                                    &candidate, &project, &output, cancelled,
                                )
                                .map_err(failure)?;
                                result["bed_preview"] =
                                    json!({"directory":output,"manifest":manifest});
                            }
                        }
                    }
                    if let Some(path) = parsed.path("--curve") {
                        let mut raw = Vec::new();
                        File::open(path)
                            .map_err(failure)?
                            .take(4 * 1024 * 1024 + 1)
                            .read_to_end(&mut raw)
                            .map_err(failure)?;
                        if raw.len() > 4 * 1024 * 1024 {
                            return Err(failure("Curve input exceeds 4 MiB"));
                        }
                        let points: Vec<[f32; 3]> =
                            serde_json::from_slice(&raw).map_err(failure)?;
                        let curve = papyrus_terrain::path_geometry::build(
                            &points,
                            source.profile.path_settings(32000000),
                            None,
                            cancelled,
                        )
                        .map_err(|e| failure(format!("Rail curve assessment failed: {e:?}")))?
                        .ok_or_else(|| failure("Rail curve assessment cancelled"))?;
                        if let Some(path) = parsed.path("--mesh-deform") {
                            let mut query = Vec::new();
                            File::open(path)
                                .map_err(failure)?
                                .take(65537)
                                .read_to_end(&mut query)
                                .map_err(failure)?;
                            if query.len() > 65536 {
                                return Err(failure("Mesh deformation query exceeds 64 KiB"));
                            }
                            let query = serde_json::from_slice(&query).map_err(failure)?;
                            let meshes = mesh_source
                                .as_ref()
                                .ok_or_else(|| failure("Rail mesh source is missing"))?;
                            result["mesh_deformation"] = meshes
                                .deform(&curve, query, source.profile.mesh_width(), cancelled)
                                .map_err(failure)?
                                .ok_or_else(|| failure("Rail mesh deformation cancelled"))?;
                        }
                        result["curve"] = json!({"kind":"diagnostic-control-path","geometry":curve,"terrain_applied":false,"world_bounds_validated":false,"socket_connections_validated":false});
                    }
                    Ok(result)
                }
                "inspect-water" => {
                    let project = Project::load(path).map_err(failure)?;
                    let root = parsed
                        .path("--rust-root")
                        .ok_or_else(|| usage("inspect-water requires --rust-root"))?;
                    let catalog = parsed
                        .path("--catalog")
                        .ok_or_else(|| usage("inspect-water requires --catalog"))?;
                    let catalog = mapper_studio::prefab_assets::load(
                        &root,
                        &catalog,
                        project
                            .installation
                            .as_ref()
                            .ok_or_else(|| failure("Recipe has no saved installation identity"))?,
                        || cancel.load(Ordering::Relaxed),
                    )
                    .map_err(failure)?;
                    let mut result =
                        mapper_studio::water_geometry::audit(&catalog, parsed.has("--details"));
                    if let Some(path) = parsed.path("--collision-cache") {
                        let cache = mapper_studio::water_collision::Cache::load(
                            &root,
                            &path,
                            &catalog,
                            || cancel.load(Ordering::Relaxed),
                        )
                        .map_err(failure)?;
                        result["collision_cache"] = cache
                            .audit(parsed.has("--details"), || cancel.load(Ordering::Relaxed))
                            .map_err(failure)?;
                    }
                    result["matches_saved_catalog"] = project
                        .monument_assets
                        .as_ref()
                        .map(|a| a.catalog_sha256 == catalog.sha256)
                        .into();
                    Ok(result)
                }
                "bind-monuments" | "inspect-monuments" => {
                    let mut project = Project::load(path).map_err(failure)?;
                    let root = parsed
                        .path("--rust-root")
                        .ok_or_else(|| usage(format!("{sub} requires --rust-root")))?;
                    let catalog_path = parsed
                        .path("--catalog")
                        .ok_or_else(|| usage(format!("{sub} requires --catalog")))?;
                    let bindings_path = parsed
                        .path("--bindings")
                        .ok_or_else(|| usage(format!("{sub} requires --bindings")))?;
                    let source_path = parsed
                        .path("--source")
                        .ok_or_else(|| usage(format!("{sub} requires --source")))?;
                    let output = if sub == "bind-monuments" {
                        Some(
                            parsed
                                .path("--output")
                                .ok_or_else(|| usage("bind-monuments requires --output"))?,
                        )
                    } else {
                        None
                    };
                    let catalog = mapper_studio::prefab_assets::load(
                        &root,
                        &catalog_path,
                        project
                            .installation
                            .as_ref()
                            .ok_or_else(|| failure("Native installation identity is missing"))?,
                        || cancel.load(Ordering::Relaxed),
                    )
                    .map_err(failure)?;
                    let bindings = mapper_studio::prefab_textures::Bindings::load(
                        &root,
                        &bindings_path,
                        &catalog,
                        || cancel.load(Ordering::Relaxed),
                    )
                    .map_err(failure)?;
                    project
                        .check_monument_assets(&catalog.sha256, &bindings.sha256)
                        .map_err(failure)?;
                    if sub == "inspect-monuments" {
                        let source = mapper_studio::monument_source::Source::load(
                            &source_path,
                            &catalog,
                            &bindings,
                            || cancel.load(Ordering::Relaxed),
                        )
                        .map_err(failure)?;
                        let mut report = source.audit(parsed.has("--details"));
                        report["matches_saved_source"] = project
                            .monument_rules
                            .as_ref()
                            .map(|r| r.source_sha256 == source.sha256)
                            .into();
                        report["source_checks_enabled"] =
                            mapper_studio::monument_source::supported(&project.placement_engine)
                                .into();
                        return Ok(report);
                    }
                    let mut source = mapper_studio::monument_source::Source::cache(
                        &source_path,
                        &catalog,
                        &bindings,
                        || cancel.load(Ordering::Relaxed),
                    )
                    .map_err(failure)?;
                    source
                        .prepare_coastal(&catalog, &root, || cancel.load(Ordering::Relaxed))
                        .map_err(failure)?;
                    if parsed.has("--stage-filters") && parsed.has("--no-stage-filters") {
                        return Err(usage("Choose either --stage-filters or --no-stage-filters"));
                    }
                    if !parsed.has("--no-stage-filters") {
                        source
                            .prepare_stages(&catalog, &root, || cancel.load(Ordering::Relaxed))
                            .map_err(failure)?;
                    }
                    let mut recipe = source.recipe();
                    if let (Some(stages), Some(previous)) = (
                        &mut recipe.stage_rules,
                        project
                            .monument_rules
                            .as_ref()
                            .and_then(|r| r.stage_rules.as_ref()),
                    ) {
                        stages.population = previous.population.clone();
                    }
                    if parsed.has("--stage-filters") && recipe.stage_rules.is_none() {
                        return Err(usage("No verified stage program adapter is available"));
                    }
                    if parsed.has("--tier-retries") && parsed.has("--no-tier-retries") {
                        return Err(usage("Choose either --tier-retries or --no-tier-retries"));
                    }
                    if parsed.has("--tier-retries") {
                        if recipe.engine != mapper_studio::coastal_placement::BUDGET_VERSION {
                            return Err(usage("Tier retries require a size-aware source recipe"));
                        }
                        recipe.tier_search =
                            Some(mapper_studio::monument_source::TIER_RETRY_VERSION.into());
                    } else if parsed.has("--no-tier-retries") {
                        recipe.tier_search = None;
                    }
                    if parsed.has("--authored-earthwork") && parsed.has("--no-authored-earthwork") {
                        return Err(usage(
                            "Choose either --authored-earthwork or --no-authored-earthwork",
                        ));
                    }
                    if parsed.has("--authored-earthwork") {
                        if recipe.engine != mapper_studio::coastal_placement::BUDGET_VERSION {
                            return Err(usage(
                                "Authored earthwork requires a size-aware source recipe",
                            ));
                        }
                        recipe.earthwork =
                            Some(mapper_studio::monument_source::AUTHORED_EARTHWORK_VERSION.into());
                    } else if parsed.has("--no-authored-earthwork") {
                        recipe.earthwork = None;
                    }
                    project.placement_engine = recipe.engine.clone();
                    project.monument_rules = Some(recipe);
                    project.monument_assets = Some(mapper_studio::AssetIdentity {
                        catalog_sha256: catalog.sha256.clone(),
                        bindings_sha256: bindings.sha256.clone(),
                    });
                    mapper_studio::generation_defaults::apply(&mut project);
                    project.validate().map_err(failure)?;
                    save_project_edit(path, output.as_ref().unwrap(), &project, parsed.has("--force"), cancel)?;
                    Ok(
                        json!({"project":output,"monument_rules":project.monument_rules,"prefabs":source.prefabs.len()}),
                    )
                }
                "bind-assets" => {
                    let mut project = Project::load(path).map_err(failure)?;
                    let root = parsed
                        .path("--rust-root")
                        .ok_or_else(|| usage("bind-assets requires --rust-root"))?;
                    let catalog = parsed
                        .path("--catalog")
                        .ok_or_else(|| usage("bind-assets requires --catalog"))?;
                    let bindings = parsed
                        .path("--bindings")
                        .ok_or_else(|| usage("bind-assets requires --bindings"))?;
                    let output = parsed
                        .path("--output")
                        .ok_or_else(|| usage("bind-assets requires --output"))?;
                    if !native_preview::is_native(&project.preview_engine) {
                        return Err(failure("Asset binding requires a native terrain recipe"));
                    }
                    let catalog = mapper_studio::prefab_assets::load(
                        &root,
                        &catalog,
                        project
                            .installation
                            .as_ref()
                            .ok_or_else(|| failure("Missing installation identity"))?,
                        || cancel.load(Ordering::Relaxed),
                    )
                    .map_err(failure)?;
                    let bindings = mapper_studio::prefab_textures::Bindings::load(
                        &root,
                        &bindings,
                        &catalog,
                        || cancel.load(Ordering::Relaxed),
                    )
                    .map_err(failure)?;
                    project.monument_assets = Some(mapper_studio::AssetIdentity {
                        catalog_sha256: catalog.sha256,
                        bindings_sha256: bindings.sha256,
                    });
                    if parsed.has("--earthwork") {
                        project.composition_engine.get_or_insert_with(|| {
                            papyrus_terrain::monument_composition::WATER_VERSION.into()
                        });
                        project.placement_engine =
                            mapper_studio::placement_preview::BOUNDS_EARTHWORK_VERSION.into();
                    }
                    if parsed.has("--rebuild-water")
                        && !papyrus_terrain::monument_composition::water_aware(
                            project.composition_engine.as_deref(),
                        )
                    {
                        project.composition_engine =
                            Some(papyrus_terrain::monument_composition::WATER_VERSION.into());
                    }
                    mapper_studio::generation_defaults::apply(&mut project);
                    project.validate().map_err(failure)?;
                    if cancel.load(Ordering::Relaxed) {
                        return Err(failure("Asset binding cancelled"));
                    }
                    save_project_edit(path, &output, &project, parsed.has("--force"), cancel)?;
                    Ok(
                        json!({"project":output,"monument_assets":project.monument_assets,"placement_engine":project.placement_engine}),
                    )
                }
                "new" => {
                    let mut project = Project::default();
                    if let Some(seed) = parsed.number("--seed")? {
                        project.master_seed = seed;
                        project.refresh_inherited_seeds();
                    }
                    if let Some(size) = parsed.number("--size")? {
                        project.size = size;
                    }
                    if let Some(root) = parsed.path("--rust-root") {
                        project.installation = Some(
                            installation::inspect(&root, || cancel.load(Ordering::Relaxed))
                                .map_err(failure)?,
                        );
                        project.preview_engine = if parsed.has("--ocean") {
                            native_preview::OCEAN_VERSION
                        } else {
                            native_preview::VERSION
                        }
                        .into();
                    } else if parsed.has("--ocean") || parsed.has("--native-profile") {
                        return Err(usage("--ocean and --native-profile require --rust-root"));
                    }
                    if let Some(path) = parsed.path("--native-profile") {
                        project.native_profile =
                            Some(native_profile::Profile::load(&path).map_err(failure)?);
                    }
                    mapper_studio::generation_defaults::apply(&mut project);
                    project.validate().map_err(failure)?;
                    save(path, &project, parsed.has("--force"))?;
                    Ok(json!({"project":path,"created":true}))
                }
                "validate" => {
                    let project = Project::load(path).map_err(failure)?;
                    let payloads=mapper_studio::project_bundle::verify(path,||cancel.load(Ordering::Relaxed)).map_err(failure)?;
                    Ok(
                        json!({"valid":true,"schema":project.schema,"terrain_engine":project.preview_engine,"saved_payloads_verified":payloads,"playable_map_validated":false}),
                    )
                }
                "inspect" => {
                    serde_json::to_value(Project::load(path).map_err(failure)?).map_err(failure)
                }
                _ => Err(usage("Unknown project command")),
            }
        }
        "native-profile" => {
            if args.get(1).and_then(|v| v.to_str()) != Some("inspect") {
                return Err(usage("Use native-profile inspect FILE"));
            }
            let parsed = Args::parse(&args[2..], &[], &[])?;
            parsed.count(1)?;
            let selected = native_profile::Profile::load(Path::new(&parsed.positionals[0]))
                .map_err(failure)?;
            Ok(
                json!({"profile":selected,"sha256":selected.sha256().map_err(failure)?,"declared_abi_verified":false}),
            )
        }
        "installation" => {
            if args.get(1).and_then(|v| v.to_str()) == Some("reader") {
                let parsed=Args::parse(&args[2..],&[],&[])?;
                parsed.count(0)?;
                return Ok(mapper_studio::asset_discovery::catalog_reader_status());
            }
            if args.get(1).and_then(|v| v.to_str()) == Some("prepare") {
                let parsed = Args::parse(&args[2..], &["--project"], &[])?;
                parsed.count(1)?;
                let root = Path::new(&parsed.positionals[0]);
                let _verification = installation::VerificationScope::default();
                let source = installation::inspect(root, || cancel.load(Ordering::Relaxed)).map_err(failure)?;
                let project = if let Some(path) = parsed.path("--project") {
                    let project = Project::load(&path).map_err(failure)?;
                    if !native_preview::is_native(&project.preview_engine)
                        || project.installation.as_ref() != Some(&source)
                    {
                        return Err(failure("Choose the Rust installation used by this project"));
                    }
                    project
                } else {
                    Project {
                        preview_engine: mapper_studio::native_preview::VERSION.into(),
                        installation: Some(source.clone()),
                        ..Default::default()
                    }
                };
                let mut previous = String::new();
                let (catalog, bindings, report) = monitor_loading(
                    || {
                        let message = preparation_progress(&loading_progress::active());
                        if !message.is_empty() && message != previous {
                            eprintln!("{message}");
                            previous = message;
                        }
                    },
                    || {
                        let _progress = loading_progress::begin("Preparing Rust installation", None);
                        mapper_studio::client_preparation::load(
                            root, &source, &project, || cancel.load(Ordering::Relaxed),
                        )
                    },
                )
                .map_err(failure)?;
                return Ok(json!({"format":"mapper-client-preparation-1","schema":1,
                    "catalog_sha256":catalog.sha256,"bindings_sha256":bindings.sha256,
                    "ready":report.steps.iter().all(|step| step.error.is_none()),
                    "steps":report.steps.iter().map(|step| json!({"name":step.name,"seconds":step.elapsed.as_secs_f64(),"error":step.error})).collect::<Vec<_>>()
                }));
            }
            if args.get(1).and_then(|v| v.to_str()) == Some("inspect-offshore") {
                let parsed = Args::parse(&args[2..], &["--catalog", "--source"], &[])?;
                parsed.count(1)?;
                let root = Path::new(&parsed.positionals[0]);
                let catalog_path = parsed
                    .path("--catalog")
                    .ok_or_else(|| usage("--catalog is required"))?;
                let installation = installation::inspect(root, || cancel.load(Ordering::Relaxed))
                    .map_err(failure)?;
                let catalog =
                    mapper_studio::prefab_assets::load(root, &catalog_path, &installation, || {
                        cancel.load(Ordering::Relaxed)
                    })
                    .map_err(failure)?;
                let (source, path) = if let Some(path) = parsed.path("--source") {
                    (
                        mapper_studio::offshore_source::Source::load(&path, root, &catalog, || {
                            cancel.load(Ordering::Relaxed)
                        })
                        .map_err(failure)?,
                        path,
                    )
                } else {
                    eprintln!(
                        "Inspecting offshore scene settings; Ctrl+C cancels. No game generation is started."
                    );
                    mapper_studio::offshore_source::Source::prepare(root, &catalog, || {
                        cancel.load(Ordering::Relaxed)
                    })
                    .map_err(failure)?
                };
                let stages=source.stages.iter().map(|stage| {
                    let roster=source.roster(stage.object_id,&catalog).map_err(failure)?;
                    Ok(json!({"program":stage,"roster":roster.iter().map(|entry|
                        json!({"path":entry.path,"search_id":entry.prefab.id,"priority_score":entry.prefab.score})).collect::<Vec<_>>()}))
                }).collect::<Result<Vec<_>,Error>>()?;
                return Ok(
                    json!({"format":"papyrus-offshore-source-inspection","schema":1,"source":path,
                    "source_sha256":source.sha256,"catalog_sha256":catalog.sha256,"stages":stages,
                    "full_stage_execution_validated":false,"candidate_search_executed":false}),
                );
            }
            if args.get(1).and_then(|v| v.to_str()) != Some("inspect") {
                return Err(usage(
                    "Use installation prepare RUST_ROOT [--project PROJECT], installation inspect RUST_ROOT, or installation inspect-offshore RUST_ROOT --catalog FILE [--source CAPTURE]",
                ));
            }
            let parsed = Args::parse(&args[2..], &[], &[])?;
            parsed.count(1)?;
            serde_json::to_value(
                installation::inspect(Path::new(&parsed.positionals[0]), || {
                    cancel.load(Ordering::Relaxed)
                })
                .map_err(failure)?,
            )
            .map_err(failure)
        }
        "recipe" | "profile" => {
            let sub = args
                .get(1)
                .and_then(|v| v.to_str())
                .ok_or_else(|| usage("Missing recipe command"))?;
            let switches: &[&str] = if sub == "import-rustmaps" { &["--force", "--base"] } else { &["--force"] };
            let parsed = Args::parse(&args[2..], switches, &["--force"])?;
            parsed.count(2)?;
            let source = Path::new(&parsed.positionals[0]);
            let output = Path::new(&parsed.positionals[1]);
            match sub {
                "export" => {
                    let value = Profile::new(
                        source
                            .file_stem()
                            .unwrap_or(OsStr::new("Settings"))
                            .to_string_lossy()
                            .into(),
                        Project::load(source).map_err(failure)?,
                    )
                    .prepare_export()
                    .map_err(failure)?;
                    save(output, &value, parsed.has("--force"))?;
                }
                "import" => {
                    let value = Profile::load(source).map_err(failure)?;
                    if output.exists() {
                        save_project_edit(output,output,&value.recipe,parsed.has("--force"),cancel)?;
                    } else {
                        save(output,&value.recipe,parsed.has("--force"))?;
                    }
                }
                "import-rustmaps" => {
                    let base = parsed.path("--base").map(|path| Project::load(&path))
                        .transpose().map_err(failure)?.unwrap_or_default();
                    let imported = mapper_studio::rustmaps_profile::load(source, &base, None).map_err(failure)?;
                    if output.exists() {
                        save_project_edit(output, output, &imported.recipe, parsed.has("--force"), cancel)?;
                    } else {
                        save(output, &imported.recipe, parsed.has("--force"))?;
                    }
                    return Ok(json!({"output":output,"written":true,"skipped":imported.skipped}));
                }
                _ => return Err(usage("Unknown recipe command")),
            }
            Ok(json!({"output":output,"written":true}))
        }
        "plugins" => {
            let sub = args
                .get(1)
                .and_then(|v| v.to_str())
                .ok_or_else(|| usage("Missing plugins command"))?;
            let parsed = Args::parse(
                &args[2..],
                match sub {
                    "run" => &[
                        "--project",
                        "--output",
                        "--parameters",
                        "--timeout",
                        "--force",
                    ],
                    "check" => &["--project", "--parameters", "--timeout"],
                    "inspect" => &[],
                    _ => return Err(usage("Unknown plugins command")),
                },
                &["--force"],
            )?;
            parsed.count(if sub == "inspect" { 1 } else { 2 })?;
            let plugin =
                plugins::Loaded::load(Path::new(&parsed.positionals[0])).map_err(failure)?;
            if sub == "inspect" {
                return Ok(
                    json!({"manifest":plugin.manifest,"fingerprint":plugin.fingerprint(),"sandboxed":false,"execution":"explicit invocation only"}),
                );
            }
            let source = parsed
                .path("--project")
                .ok_or_else(|| usage("Plugin actions require --project"))?;
            let output = (sub == "run").then(|| {
                parsed.path("--output").ok_or_else(|| usage("plugins run requires --output"))
            }).transpose()?;
            if output.as_ref().is_some_and(|path| path.exists()) && !parsed.has("--force") {
                return Err(failure("Plugin output exists; use another path or --force"));
            }
            let edit = mapper_studio::project_bundle::RecipeEdit::open(&source).map_err(failure)?;
            let parameters = if let Some(path) = parsed.path("--parameters") {
                let mut bytes = Vec::new();
                File::open(path)
                    .map_err(failure)?
                    .take(65537)
                    .read_to_end(&mut bytes)
                    .map_err(failure)?;
                if bytes.len() > 65536 {
                    return Err(failure("Plugin parameters exceed 64 KiB"));
                }
                serde_json::from_slice(&bytes).map_err(failure)?
            } else {
                json!({})
            };
            let seconds = parsed.number("--timeout")?.unwrap_or(10);
            if !(1..=60).contains(&seconds) {
                return Err(usage("--timeout must be 1..60 seconds"));
            }
            let action = text(&parsed.positionals[1])?;
            let timeout = Duration::from_secs(u64::from(seconds));
            let Some(output) = output else {
                let result = plugin.invoke(
                    action, plugins::Capability::Inspect, &edit.recipe, parameters, timeout, cancel,
                ).map_err(failure)?;
                let plugins::Outcome::Report(report) = result else {
                    return Err(failure("Expected a plugin report"));
                };
                edit.ensure_current().map_err(failure)?;
                if cancel.load(Ordering::Relaxed) {
                    return Err(failure("Plugin cancelled"));
                }
                return Ok(json!({"plugin":plugin.manifest.id,"action":action,"report":report}));
            };
            let changed = plugin
                .run(action, &edit.recipe, parameters, timeout, cancel)
                .map_err(failure)?;
            let modified = changed != edit.recipe;
            edit.save(&output, changed, parsed.has("--force"), || cancel.load(Ordering::Relaxed))
                .map_err(failure)?;
            Ok(json!({"output":output,"plugin":plugin.manifest.id,"changed":modified}))
        }
        _ => Err(usage("Unknown command; run mapper --help")),
    }
}
fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        let _ = std::io::stdout().write_all(HELP.as_bytes());
        return ExitCode::SUCCESS;
    }
    if args == [OsString::from("--version")] {
        println!("Mapper for Rust {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let signal = cancel.clone();
    if let Err(error) = ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed)) {
        eprintln!(
            "{}",
            json!({"error":format!("Cannot register cancellation: {error}")})
        );
        return ExitCode::FAILURE;
    }
    match run(&args, &cancel) {
        Ok(value) => {
            let stdout = std::io::stdout();
            let mut output = stdout.lock();
            if serde_json::to_writer(&mut output, &value)
                .and_then(|_| output.write_all(b"\n").map_err(serde_json::Error::io))
                .is_err()
            {
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            let code = if cancel.load(Ordering::Relaxed) {
                130
            } else {
                error.code
            };
            eprintln!("{}", json!({"error":error.message,"exit_code":code}));
            ExitCode::from(code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preparation_progress_groups_parallel_reads_under_one_stage() {
        let job = |stage: &str, item: &str| Snapshot {
            id: 0,
            label: String::new(),
            stage: stage.into(),
            item: item.into(),
            completed: 0,
            total: None,
            unit: loading_progress::Unit::Bytes,
            elapsed: Duration::ZERO,
            can_cancel: false,
            cancelling: false,
        };
        let jobs = [
            job("Verifying Rust files", ""),
            job("Verifying Rust files", "Bundles/shared/content.bundle"),
            job("Verifying Rust files", r"Bundles\maps\maps.bundle"),
            job("Preparing generation data", ""),
        ];
        assert_eq!(
            preparation_progress(&jobs),
            "Verifying Rust files · content.bundle, maps.bundle"
        );
    }

    #[test]
    fn erosion_progress_uses_actual_iteration_milestones() {
        let progress = loading_progress::begin("CLI erosion fixture", None);
        let reporter = progress.reporter();
        let current = || {
            loading_progress::active()
                .into_iter()
                .find(|job| job.label == "CLI erosion fixture")
                .unwrap()
        };
        let mut last = None;
        assert_eq!(erosion_milestone(&current(), &mut last), None);
        reporter.stage("Simulating erosion");
        assert_eq!(erosion_milestone(&current(), &mut last), None);
        for (done, expected) in [
            (0, Some((0, 512))),
            (1, Some((1, 512))),
            (2, None),
            (52, Some((52, 512))),
            (53, None),
            (511, Some((511, 512))),
            (512, Some((512, 512))),
        ] {
            reporter.update("", done, Some(512), loading_progress::Unit::Items);
            assert_eq!(erosion_milestone(&current(), &mut last), expected);
        }
        reporter.stage("Saving erosion checkpoint");
        assert_eq!(erosion_milestone(&current(), &mut last), None);
    }

    #[test]
    fn map_only_cli_checks_its_destination_and_source_arguments() {
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("source.mapper");
        save(&project, &Project::default(), false).unwrap();
        let command = ["generate".into(), project.into_os_string()];
        let cancel = AtomicBool::new(false);
        let missing_output = run(&command, &cancel).unwrap_err();
        assert_eq!(missing_output.code, 2);
        assert!(missing_output.message.contains("--map FILE"));

        let map = directory.path().join("world.map");
        let mut command = command.to_vec();
        command.extend(["--map".into(), map.clone().into_os_string()]);
        let missing_sources = run(&command, &cancel).unwrap_err();
        assert_eq!(missing_sources.code, 2);
        assert!(missing_sources.message.contains("--map requires composed native generation"));
        assert!(!map.exists());

        let mut invalid_workers = command.clone();
        invalid_workers.extend(["--rust-root".into(), ".".into(), "--cpu-workers".into(), "0".into()]);
        let invalid_workers = run(&invalid_workers, &cancel).unwrap_err();
        assert_eq!(invalid_workers.code, 2);
        assert!(invalid_workers.message.contains("--cpu-workers requires a count"));

        std::fs::write(&map, b"retained").unwrap();
        let already_exists = run(&command, &cancel).unwrap_err();
        assert_eq!(already_exists.code, 1);
        assert_eq!(already_exists.message, "Map output already exists");
        assert_eq!(std::fs::read(map).unwrap(), b"retained");
    }

    #[test]
    fn requested_export_keeps_inspectable_previews_for_incomplete_worlds() {
        use eframe::egui::{Color32, ColorImage};
        use papyrus_terrain::placement::{Outcome, Solution};
        let recipe = Project::default();
        let mut generated = headless::Generated {
            image: ColorImage::filled([2, 2], Color32::GREEN),
            heights: vec![16000; 4],
            resolution: 2,
            native: None,
            composed: None,
            cables: None,
            independent_biomes: None,
            solution: Solution {
                outcomes: recipe
                    .targets
                    .iter()
                    .map(|target| Outcome {
                        id: target.instance_id,
                        position: None,
                        distance: None,
                        within_radius: false,
                        reason: "No valid ground".into(),
                    })
                    .collect(),
                required_missing: recipe.targets.len(),
                attempts: 0,
            },
            asset_receipts: None,
            warnings: Vec::new(),
            recipe,
        };
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("preview");
        let result = save_generation_preview(
            &mut generated,
            &output,
            true,
            &AtomicBool::new(false),
            |_| {},
        );
        let error = result.unwrap_err();
        assert_eq!(error.code, 1);
        assert!(error.message.contains(&generated.recipe.targets[0].name));
        assert!(error.message.contains("Preview and recipe saved"));
        assert_eq!(
            Project::load(&output.join("project.mapper")).unwrap(),
            generated.recipe
        );
        assert!(output.join("overview.png").is_file());
        assert!(output.join("layout.json").is_file());

        generated.recipe.targets.clear();
        generated.solution.outcomes.clear();
        generated.solution.required_missing = 0;
        generated.warnings.clear();
        let output = directory.path().join("incomplete-composition");
        let error = save_generation_preview(
            &mut generated,
            &output,
            true,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap_err();
        let issue = "Map export requires the accepted composed world";
        assert_eq!(error.code, 1);
        assert!(error.message.starts_with(issue));
        assert!(error.message.contains("Preview and recipe saved"));
        assert_eq!(
            Project::load(&output.join("project.mapper")).unwrap(),
            generated.recipe
        );
        let manifest: Value =
            serde_json::from_slice(&std::fs::read(output.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["warnings"], json!([issue]));
        assert_eq!(manifest["playable"], false);
        assert!(output.join("overview.png").is_file());

        let error = save_generation_preview(
            &mut generated,
            &output,
            true,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap_err();
        assert!(error.message.starts_with(issue));
        assert!(error.message.contains("Could not save preview"));
        assert_eq!(generated.warnings, [issue]);
    }

    #[test]
    fn project_cli_edits_keep_saved_payloads_and_validate_their_checksums() {
        use mapper_studio::project_bundle::{self, Source};
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.mapper");
        let output = directory.path().join("updated.mapper");
        let payload = Source::Bytes(Arc::from(b"retained world payload".as_slice()));
        project_bundle::write(&source, Project::default(), Value::Null, &[payload], || {
            false
        })
        .unwrap();
        let cancel = AtomicBool::new(false);
        run(
            &[
                "project".into(),
                "disable-rivers".into(),
                source.as_os_str().to_owned(),
                "--output".into(),
                output.as_os_str().to_owned(),
            ],
            &cancel,
        )
        .unwrap();
        let loaded = project_bundle::load(&output, || false).unwrap().unwrap();
        assert_eq!(
            std::fs::read(loaded.blobs[0].as_ref()).unwrap(),
            b"retained world payload"
        );
        drop(loaded);
        let validate = [
            "project".into(),
            "validate".into(),
            output.as_os_str().to_owned(),
        ];
        assert_eq!(
            run(&validate, &cancel).unwrap()["saved_payloads_verified"],
            1
        );
        let mut bytes = std::fs::read(&output).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        std::fs::write(&output, bytes).unwrap();
        assert_eq!(run(&validate, &cancel).unwrap_err().code, 1);
        let profile = directory.path().join("settings.rec");
        Profile::new(
            "Settings".into(),
            Project {
                size: 2000,
                ..Default::default()
            },
        )
        .save(&profile)
        .unwrap();
        run(
            &[
                "recipe".into(),
                "import".into(),
                profile.as_os_str().to_owned(),
                source.as_os_str().to_owned(),
                "--force".into(),
            ],
            &cancel,
        )
        .unwrap();
        assert_eq!(Project::load(&source).unwrap().size, 2000);
        assert_eq!(project_bundle::verify(&source, || false).unwrap(), Some(1));
    }
    #[test]
    #[ignore = "requires an owned installation and source-bound road preparation inputs"]
    fn owned_native_road_binding_validates_global_earthwork_after_ordered_inputs() {
        let input = |name| std::env::var_os(name).expect(name);
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("bound.mapper");
        let mut command = args(&["project", "bind-roads"]);
        command.push(input("MAPPER_ROAD_BIND_PROJECT"));
        for (flag, name) in [
            ("--rust-root", "MAPPER_ROAD_BIND_ROOT"),
            ("--catalog", "MAPPER_ROAD_BIND_CATALOG"),
            ("--source", "MAPPER_ROAD_BIND_SOURCE"),
            ("--surfaces", "MAPPER_ROAD_BIND_SURFACES"),
            ("--staged-roadside", "MAPPER_ROAD_BIND_ROADSIDE"),
            ("--ring-costmap-profile", "MAPPER_ROAD_BIND_COSTS"),
            ("--access-costmap-profile", "MAPPER_ROAD_BIND_COSTS"),
        ] {
            command.push(flag.into());
            command.push(input(name));
        }
        command.extend(args(&["--ordered-road-layout", "--global-road-earthwork", "--output"]));
        command.push(output.clone().into_os_string());
        run(&command, &AtomicBool::new(false)).unwrap_or_else(|error| panic!("{}", error.message));
        let project = Project::load(&output).unwrap();
        let roads = project.roads.unwrap();
        assert!(roads.staged.unwrap().layout.is_some());
        assert_eq!(
            roads.entrance_launch.unwrap().engine,
            papyrus_terrain::road_network::LaunchEngine::GlobalEarthwork
        );
        assert_eq!(
            roads.access_costmap.unwrap().policy.sampling.as_deref(),
            Some(papyrus_terrain::road_costmap::NATIVE_SAMPLING_VERSION)
        );
    }
    #[test]
    fn terrain_entrance_flags_reject_conflicts_before_loading_a_project() {
        for flags in [
            ["--terrain-road-entrances", "--no-terrain-road-entrances"],
            ["--terrain-road-entrances", "--legacy-road-layout"],
            ["--terrain-road-entrances", "--no-staged-roads"],
        ] {
            let mut command = args(&["project", "bind-roads", "nonexistent.mapper"]);
            command.extend(args(&flags));
            let error = run(&command, &AtomicBool::new(false)).unwrap_err();
            assert_eq!(error.code, 2);
            assert!(
                error
                    .message
                    .contains("generation correctness rules are automatic")
            );
        }
    }
    #[test]
    fn ordered_layout_flags_are_booleans_and_conflict_before_loading_inputs() {
        for flags in [
            ["--ordered-road-layout", "--legacy-road-layout"],
            ["--ordered-road-layout", "--no-staged-roads"],
        ] {
            let mut command = args(&["project", "bind-roads", "nonexistent.mapper"]);
            command.extend(args(&flags));
            let error = run(&command, &AtomicBool::new(false)).unwrap_err();
            assert_eq!(error.code, 2);
            assert!(
                error
                    .message
                    .contains("generation correctness rules are automatic")
            );
        }
        assert!(!HELP.contains("--legacy-road-layout"));
    }
    #[test]
    fn shared_earthwork_flags_conflict_before_loading_a_project() {
        for flags in [
            ["--global-road-earthwork", "--no-global-road-earthwork"],
            ["--global-road-earthwork", "--no-staged-roads"],
            ["--global-road-earthwork", "--no-connected-exits"],
            [
                "--constrained-road-smoothing",
                "--no-constrained-road-smoothing",
            ],
        ] {
            let mut command = args(&["project", "bind-roads", "nonexistent.mapper"]);
            command.extend(args(&flags));
            let error = run(&command, &AtomicBool::new(false))
                .expect_err("Conflicting road choices accepted");
            assert_eq!(error.code, 2);
            assert!(
                error
                    .message
                    .contains("generation correctness rules are automatic")
            );
        }
        assert!(!HELP.contains("--no-global-road-earthwork"));
    }
    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }
    #[test]
    fn reader_status_needs_no_game_folder_and_rejects_extra_arguments() {
        let value=run(&args(&["installation","reader"]),&AtomicBool::new(false)).unwrap_or_else(|error|panic!("{}",error.message));
        assert_eq!(value["format"],"mapper-native-catalog-reader-status-1");
        assert_eq!(value["extractor_sha256"].as_str().unwrap().len(),64);
        assert!(value["reader_present"].is_boolean());
        for input in [vec!["installation","reader","unexpected"],vec!["installation","reader","--unknown"]] {
            assert_eq!(run(&args(&input),&AtomicBool::new(false)).unwrap_err().code,2);
        }
        assert!(HELP.contains("mapper installation reader"));
    }
    #[test]
    fn installation_preparation_rejects_invalid_arguments_before_reading_files() {
        for input in [
            vec!["installation", "prepare"],
            vec!["installation", "prepare", "missing-root", "--unknown"],
        ] {
            assert_eq!(run(&args(&input), &AtomicBool::new(false)).unwrap_err().code, 2);
        }
        assert!(HELP.contains("installation prepare RUST_ROOT"));
    }

    #[test]
    fn offshore_inspection_checks_required_and_unknown_options_before_installation_io() {
        for input in [
            vec!["installation", "inspect-offshore", "missing-root"],
            vec![
                "installation",
                "inspect-offshore",
                "missing-root",
                "--unknown",
                "value",
            ],
            vec![
                "installation",
                "inspect-offshore",
                "missing-root",
                "--catalog",
            ],
        ] {
            let error = run(&args(&input), &AtomicBool::new(false)).err().unwrap();
            assert_eq!(error.code, 2);
        }
        assert!(
            HELP.contains(
                "installation inspect-offshore RUST_ROOT --catalog FILE [--source CAPTURE]"
            )
        );
    }
    #[test]
    fn river_preparation_refuses_existing_output_before_installation_work() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project.mapper");
        Project::default().save(&project).unwrap();
        let output = dir.path().join("existing.mapper");
        std::fs::write(&output, b"keep").unwrap();
        let result = run(
            &[
                "project".into(),
                "prepare-rivers".into(),
                project.into_os_string(),
                "--rust-root".into(),
                dir.path().join("missing").into_os_string(),
                "--output".into(),
                output.clone().into_os_string(),
            ],
            &AtomicBool::new(false),
        );
        assert!(
            result
                .err()
                .unwrap()
                .message
                .contains("Output already exists")
        );
        assert_eq!(std::fs::read(output).unwrap(), b"keep");
    }
    #[test]
    fn road_effect_modes_require_a_bundle_and_cannot_be_combined() {
        for options in [
            vec!["--live-effects-output", "new"],
            vec![
                "--effects-output",
                "old",
                "--live-effects-output",
                "new",
                "--terrain-bundle",
                "bundle",
            ],
        ] {
            let mut command = args(&["project", "inspect-road-objects", "nonexistent.mapper"]);
            command.extend(args(&options));
            let error = run(&command, &AtomicBool::new(false)).expect_err("invalid mode accepted");
            assert_eq!(error.code, 2);
            assert!(error.message.contains("--effects-output"));
        }
    }
    #[test]
    fn rail_preparation_refuses_existing_output_and_bad_settings_before_game_asset_work() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project.mapper");
        Project::default().save(&project).unwrap();
        let output = root.path().join("output.mapper");
        std::fs::write(&output, b"preserve").unwrap();
        let settings = root.path().join("settings.json");
        std::fs::write(&settings, b"{}").unwrap();
        let command = vec![
            "project".into(),
            "prepare-rails".into(),
            project.into_os_string(),
            "--rust-root".into(),
            root.path().join("missing-game").into_os_string(),
            "--catalog".into(),
            root.path().join("missing-catalog").into_os_string(),
            "--bindings".into(),
            root.path().join("missing-bindings").into_os_string(),
            "--settings".into(),
            settings.into_os_string(),
            "--output".into(),
            output.clone().into_os_string(),
        ];
        let error = run(&command, &AtomicBool::new(false)).unwrap_err();
        assert!(error.message.contains("Output already exists"));
        assert_eq!(std::fs::read(&output).unwrap(), b"preserve");
        let mut forced = command;
        forced.push("--force".into());
        let error = run(&forced, &AtomicBool::new(false)).unwrap_err();
        assert!(error.message.contains("missing field"), "{}", error.message);
        assert_eq!(std::fs::read(output).unwrap(), b"preserve");
    }
    #[test]
    fn options_reject_duplicates_unknown_and_missing_values() {
        assert!(
            Args::parse(
                &args(&["--size", "1000", "--size", "2000"]),
                &["--size"],
                &[]
            )
            .is_err()
        );
        assert!(Args::parse(&args(&["--typo"]), &[], &[]).is_err());
        assert!(Args::parse(&args(&["--size"]), &["--size"], &[]).is_err());
        let parsed = Args::parse(&args(&["--", "--filename"]), &[], &[])
            .unwrap_or_else(|_| panic!("Literal path should parse"));
        assert_eq!(parsed.positionals, args(&["--filename"]));
    }
    #[test]
    fn monument_policy_switches_are_value_free_and_stage_switches_are_command_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("project.mapper");
        Project::default().save(&path).unwrap();
        for flag in [
            "--stage-filters",
            "--no-stage-filters",
            "--tier-retries",
            "--no-tier-retries",
            "--authored-earthwork",
            "--no-authored-earthwork",
        ] {
            let error = run(
                &[
                    "project".into(),
                    "bind-monuments".into(),
                    path.clone().into_os_string(),
                    flag.into(),
                ],
                &AtomicBool::new(false),
            )
            .unwrap_err();
            assert_eq!(error.code, 2);
            assert!(
                error.message.contains(if flag.starts_with("--no-") {
                    "generation correctness rules are automatic"
                } else {
                    "bind-monuments requires --rust-root"
                }),
                "{flag}: {}",
                error.message
            );
        }
        let error = run(
            &[
                "project".into(),
                "inspect-rails".into(),
                path.into_os_string(),
                "--stage-filters".into(),
            ],
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert_eq!(error.code, 2);
        assert!(error.message.contains("Unknown option: --stage-filters"));
    }
    #[test]
    fn native_profile_inspection_is_data_only_and_validates_abi_contract() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("native-profile.json");
        std::fs::write(
            &path,
            include_bytes!("../../../../compatibility/native/client-0f703d43.json"),
        )
        .unwrap();
        let command = vec![
            "native-profile".into(),
            "inspect".into(),
            path.clone().into_os_string(),
        ];
        let result =
            run(&command, &AtomicBool::new(false)).unwrap_or_else(|e| panic!("{}", e.message));
        assert_eq!(result["declared_abi_verified"], false);
        assert_eq!(result["profile"]["abi"], native_profile::ABI);
        let mut invalid = result["profile"].clone();
        invalid["abi"] = "unknown".into();
        std::fs::write(&path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(run(&command, &AtomicBool::new(false)).is_err());
    }
    #[test]
    fn project_and_profile_roundtrip_refuse_unrequested_overwrite() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("new.mapper");
        let command = vec![
            "project".into(),
            "new".into(),
            project.clone().into_os_string(),
            "--seed".into(),
            "54321".into(),
        ];
        let cancel = AtomicBool::new(false);
        assert!(run(&command, &cancel).is_ok());
        assert_eq!(Project::load(&project).unwrap().master_seed, 54321);
        assert!(run(&command, &cancel).is_err());
        let profile = root.path().join("settings.rec");
        let restored = root.path().join("restored.mapper");
        assert!(
            run(
                &[
                    "profile".into(),
                    "export".into(),
                    project.clone().into_os_string(),
                    profile.clone().into_os_string()
                ],
                &cancel
            )
            .is_ok()
        );
        assert!(
            run(
                &[
                    "profile".into(),
                    "import".into(),
                    profile.into_os_string(),
                    restored.clone().into_os_string()
                ],
                &cancel
            )
            .is_ok()
        );
        assert_eq!(
            Project::load(&restored).unwrap(),
            Project::load(&project).unwrap()
        );
    }
}
