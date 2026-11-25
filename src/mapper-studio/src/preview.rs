use eframe::egui::{Color32, ColorImage};
use mapper_studio::{Project, TerrainKey};
use papyrus_terrain::{Settings, generate};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver},
};
use std::time::{Duration, Instant};

#[derive(Clone, Default)]
pub struct Metrics {
    pub elapsed_ms: f64,
    pub snapshot_ms: f64,
    pub snapshots: usize,
    pub ocean_ms: f64,
    pub reused_native_source: bool,
    pub terrain_cache: String,
}
pub struct Progress {
    pub key: TerrainKey,
    pub label: &'static str,
    pub image: Option<ColorImage>,
}

fn store_progress(slot: &Mutex<Option<Progress>>, mut update: Progress) {
    if let Ok(mut slot) = slot.lock() {
        if update.image.is_none() {
            update.image = slot
                .as_mut()
                .filter(|old| old.key == update.key)
                .and_then(|old| old.image.take());
        }
        *slot = Some(update);
    }
}

#[derive(Clone)]
pub struct Preview {
    pub ordered_terrain: bool,
    pub raw_source: Option<Arc<mapper_studio::native_processing::Source>>,
    pub rivers: Option<Arc<papyrus_terrain::river_pipeline::World>>,
    pub key: TerrainKey,
    pub image: Arc<ColorImage>,
    pub display_image: Option<Arc<ColorImage>>,
    pub landscape: Option<Arc<papyrus_terrain::landscape::Landscape>>,
    pub heights: Arc<Vec<i16>>,
    pub resolution: usize,
    pub metrics: Metrics,
    pub palette: Option<Arc<mapper_studio::terrain_palette::Palette>>,
    pub native_layers: Option<Arc<papyrus_terrain::native_layers::Layers>>,
    pub surface: Option<Arc<mapper_studio::map_surface_frame::Frame>>,
    pub ocean: Option<Arc<papyrus_terrain::ocean::Ocean>>,
}
impl Preview {
    pub fn matches_project(&self, project: &Project) -> bool {
        self.key == project.terrain_key()
            && self.ordered_terrain
                == mapper_studio::native_processing::rivers_follow_terrain(project)
    }

    pub fn viewport_image(&self) -> &Arc<ColorImage> {
        self.display_image.as_ref().unwrap_or(&self.image)
    }

    fn raw_native_source(&self) -> Option<mapper_studio::native_processing::Source> {
        if let Some(raw) = &self.raw_source {
            return Some(raw.as_ref().clone());
        }
        if self.rivers.is_some() || self.key.rivers.is_some() {
            return None;
        }
        self.native_layers.as_ref().map(|layers| mapper_studio::native_processing::Source {
            terrain: None,
            native_base: None,
            rivers: None,
            palette: self.palette.clone(),
            key: mapper_studio::native_preview::source_key(&self.key),
            image: self.image.as_ref().clone(),
            heights: self.heights.clone(),
            layers: layers.clone(),
        })
    }
}

pub struct Task {
    pub ordered_terrain: bool,
    pub key: TerrainKey,
    pub receiver: Receiver<Result<Option<Preview>, String>>,
    pub cancel: Arc<AtomicBool>,
    pub progress: Arc<Mutex<Option<Progress>>>,
}

#[derive(Clone, Copy)]
pub(super) struct Presentation {
    pub live: bool,
    pub detailed: bool,
}

pub(super) fn start_worker<T: Send + 'static>(
    name: &str,
    ctx: eframe::egui::Context,
    work: impl FnOnce(&eframe::egui::Context) -> Result<Option<T>, String> + Send + 'static,
) -> Receiver<Result<Option<T>, String>> {
    let (sender, receiver) = mpsc::channel();
    let worker_sender = sender.clone();
    let worker_ctx = ctx.clone();
    if let Err(error) = std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let _ = worker_sender.send(work(&worker_ctx));
            worker_ctx.request_repaint();
        })
    {
        let _ = sender.send(Err(format!("Could not start worker: {error}")));
        ctx.request_repaint();
    }
    receiver
}

impl Task {
    pub fn start(
        project: &Project,
        presentation: Presentation,
        root: Option<std::path::PathBuf>,
        cached: Option<&Preview>,
        prepared_palette: Option<Arc<mapper_studio::terrain_palette::Palette>>,
        reuse_render: bool,
        ctx: eframe::egui::Context,
    ) -> Self {
        let key = project.terrain_key();
        let native = mapper_studio::native_preview::is_native(&key.engine);
        let ordered_terrain = mapper_studio::native_processing::rivers_follow_terrain(project);
        let cached = cached.filter(|_| native).cloned();
        let project = project.clone();
        let task_key = key.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&cancel);
        let progress = Arc::new(Mutex::new(None));
        let progress_sink = Arc::clone(&progress);
        let receiver = start_worker("mapper-preview", ctx, move |ctx| {
            let _verification = mapper_studio::installation::VerificationScope::default();
            let mut notify = |update| {
                store_progress(&progress_sink, update);
                ctx.request_repaint();
            };
            if native {
                (|| {
                    if reuse_render
                        && let Some((retained, root)) = cached.as_ref().zip(root.as_deref())
                        && retained.native_layers.is_some()
                        && retained.matches_project(&project)
                        && (!presentation.detailed || retained.display_image.is_some())
                        // Legacy previews have already applied rivers. Their
                        // pinned inputs are rechecked by normal processing.
                        && (key.rivers.is_none() || retained.ordered_terrain)
                    {
                        notify(Progress {
                            key: key.clone(),
                            label: "Checking cached terrain",
                            image: None,
                        });
                        let started = Instant::now();
                        if !mapper_studio::native_preview::verify_source(root, &key, &signal)? {
                            return Ok(None);
                        }
                        if let Some(palette) = &retained.palette
                            && prepared_palette.as_ref().is_none_or(|prepared| {
                                prepared.source_render_key() == palette.source_render_key()
                            })
                            && let Some(installation) = &key.source
                            && mapper_studio::native_processing::prepared_palette_matches(
                                root, installation, palette, || signal.load(Ordering::Relaxed),
                            )
                        {
                            let mut preview = retained.clone();
                            preview.metrics = Metrics {
                                elapsed_ms: started.elapsed().as_secs_f64() * 1000.,
                                reused_native_source: true,
                                terrain_cache: "Reused verified terrain preview".into(),
                                ..Default::default()
                            };
                            return Ok(Some(preview));
                        }
                    }
                    build_native(
                        &project,
                        root.as_deref(),
                        cached.as_ref().and_then(Preview::raw_native_source),
                        prepared_palette,
                        &signal,
                        presentation,
                        &mut notify,
                    )
                })()
            } else {
                build(&key, &signal, presentation.live, notify)
            }
        });
        Self {
            ordered_terrain,
            key: task_key,
            receiver,
            cancel,
            progress,
        }
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn build_native(
    project: &Project,
    root: Option<&std::path::Path>,
    cached: Option<mapper_studio::native_processing::Source>,
    prepared_palette: Option<Arc<mapper_studio::terrain_palette::Palette>>,
    cancel: &AtomicBool,
    presentation: Presentation,
    mut notify: impl FnMut(Progress),
) -> Result<Option<Preview>, String> {
    let key = project.terrain_key();
    let root = root.ok_or(
        "Select a matching Rust folder to bind this project before generating native terrain",
    )?;
    let started = Instant::now();
    let mut snapshots = 0;
    let native = mapper_studio::native_processing::build_for_project_with_snapshots(
        root,
        &mapper_studio::native_preview::worker_path()?,
        project,
        cached,
        prepared_palette,
        cancel,
        presentation.live,
        |label, _, image| {
            snapshots += usize::from(image.is_some());
            notify(Progress {
                key: key.clone(),
                label,
                image,
            })
        },
    )?;
    let Some(native) = native else {
        return Ok(None);
    };
    finish_native(project, native, presentation.detailed, cancel, started, snapshots, notify)
}

fn finish_native(
    project: &Project,
    native: mapper_studio::native_processing::Processed,
    detailed: bool,
    cancel: &AtomicBool,
    started: Instant,
    snapshots: usize,
    mut notify: impl FnMut(Progress),
) -> Result<Option<Preview>, String> {
    if cancel.load(Ordering::Relaxed) {
        return Ok(None);
    }
    let key = project.terrain_key();
    let (display_image, surface) = if detailed {
        notify(Progress {
            key: key.clone(),
            label: "Rendering detailed map",
            image: None,
        });
        let Some(display) = mapper_studio::map_image::render_native_viewport(
            &native.source.layers,
            native.ocean.as_deref(),
            native.source.palette.as_deref(),
            || cancel.load(Ordering::Relaxed),
        )?
        else {
            return Ok(None);
        };
        (Some(Arc::new(display.image)), display.surface)
    } else {
        (None, None)
    };
    Ok(Some(Preview {
        ordered_terrain: mapper_studio::native_processing::rivers_follow_terrain(project),
        raw_source: native.raw_source.map(Arc::new),
        rivers: native.source.rivers,
        key,
        image: Arc::new(native.source.image),
        display_image,
        surface,
        landscape: None,
        heights: native.source.heights,
        resolution: mapper_studio::native_preview::RESOLUTION,
        metrics: Metrics {
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.,
            snapshots,
            ocean_ms: native.ocean_ms,
            reused_native_source: native.reused_source,
            terrain_cache: native.cache_status,
            ..Default::default()
        },
        palette: native.source.palette,
        native_layers: Some(native.source.layers),
        ocean: native.ocean,
    }))
}

fn build(
    key: &TerrainKey,
    cancel: &AtomicBool,
    live: bool,
    notify: impl FnMut(Progress),
) -> Result<Option<Preview>, String> {
    build_with_interval(key, cancel, live, Duration::from_millis(160), notify)
}

fn build_with_interval(
    key: &TerrainKey,
    cancel: &AtomicBool,
    live: bool,
    interval: Duration,
    mut notify: impl FnMut(Progress),
) -> Result<Option<Preview>, String> {
    let started = Instant::now();
    if cancel.load(Ordering::Relaxed) {
        return Ok(None);
    }
    notify(Progress {
        key: key.clone(),
        label: "Starting terrain generation",
        image: None,
    });
    if key.engine == mapper_studio::LEGACY_PREVIEW_ENGINE {
        return build_legacy(key, cancel).map(|r| {
            r.map(|mut p| {
                p.metrics.elapsed_ms = started.elapsed().as_secs_f64() * 1000.;
                p
            })
        });
    }
    if key.engine != papyrus_terrain::landscape::VERSION {
        return Err("Unsupported preview engine.".into());
    }
    let settings = papyrus_terrain::landscape::Settings {
        terrain_seed: key.seed as i32,
        biome_seed: key.biome_seed.ok_or("Missing independent biome seed")? as i32,
        resolution: 513,
        size: key.size as usize,
    };
    let mut metrics = Metrics::default();
    let mut last_snapshot = started;
    let landscape = mapper_studio::generation_resources::with_cpu_pool(
        || cancel.load(Ordering::Relaxed),
        |pool, _| {
            papyrus_terrain::landscape::generate_with_pool(
                settings,
                pool,
                |stage, land| {
                    let mut image = None;
                    if live
                        && !cancel.load(Ordering::Relaxed)
                        && last_snapshot.elapsed() >= interval
                    {
                        let clock = Instant::now();
                        image = mapper_studio::terrain_view::render(
                            land,
                            key.size,
                            key.biome_seed.unwrap_or(key.seed),
                            129,
                        )
                        .ok();
                        metrics.snapshot_ms += clock.elapsed().as_secs_f64() * 1000.;
                        metrics.snapshots += usize::from(image.is_some());
                        last_snapshot = Instant::now();
                    }
                    notify(Progress {
                        key: key.clone(),
                        label: stage.label(),
                        image,
                    });
                },
                || cancel.load(Ordering::Relaxed),
            )
        },
    )?;
    let Some(land) = landscape else {
        return Ok(None);
    };
    notify(Progress {
        key: key.clone(),
        label: "Drawing terrain map",
        image: None,
    });
    let image =
        mapper_studio::terrain_view::render(&land, key.size, key.biome_seed.unwrap_or(key.seed), 513)?;
    if cancel.load(Ordering::Relaxed) {
        return Ok(None);
    }
    let Some(display_image) = mapper_studio::terrain_view::render_cancellable(
        &land,
        key.size,
        key.biome_seed.unwrap_or(key.seed),
        mapper_studio::map_image::DISPLAY_RESOLUTION,
        || cancel.load(Ordering::Relaxed),
    )?
    else {
        return Ok(None);
    };
    if cancel.load(Ordering::Relaxed) {
        return Ok(None);
    }
    let heights = land
        .heights
        .iter()
        .map(|&h| ((h + 500.) / 1000. * 32766. + 0.5).clamp(0., 32766.) as i16)
        .collect();
    metrics.elapsed_ms = started.elapsed().as_secs_f64() * 1000.;
    let resolution = land.resolution;
    Ok(Some(Preview {
        ordered_terrain: false,
        raw_source: None,
        rivers: None,
        key: key.clone(),
        image: Arc::new(image),
        display_image: Some(Arc::new(display_image)),
        landscape: Some(Arc::new(land)),
        heights: Arc::new(heights),
        resolution,
        metrics,
        palette: None,
        native_layers: None,
        surface: None,
        ocean: None,
    }))
}

fn build_legacy(key: &TerrainKey, cancel: &AtomicBool) -> Result<Option<Preview>, String> {
    let seed = key.seed;
    let n = 513;
    let Some(field) = mapper_studio::generation_resources::with_cpu_pool(
        || cancel.load(Ordering::Relaxed),
        |_, limits| {
            generate(Settings {
                seed: seed as i32,
                resolution: n,
                threads: limits.effective_workers(),
                biomes: [40, 15, 15, 30],
            })
            .map(Some)
        },
    )?
    else {
        return Ok(None);
    };
    let palette = [
        [26., 65., 77.],
        [182., 165., 116.],
        [112., 143., 91.],
        [146., 151., 129.],
        [223., 233., 230.],
    ];
    let height = |x: usize, z: usize| field.heights[z * n + x];
    let mut pixels = Vec::with_capacity(n * n);
    for row in 0..n {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let z = n - 1 - row;
        for x in 0..n {
            let h = height(x, z);
            let mut c = palette[usize::from(field.biomes[z * n + x]).min(4)];
            if h <= 0.08 {
                let d = ((0.08 - h) * 12.).clamp(0., 1.);
                c = [80. - d * 60., 135. - d * 74., 138. - d * 65.];
            } else {
                let dx = height(x.saturating_sub(1), z) - height((x + 1).min(n - 1), z);
                let dz = height(x, z.saturating_sub(1)) - height(x, (z + 1).min(n - 1));
                let light = (0.9 + dx * 22. + dz * 12.).clamp(0.55, 1.2);
                c = c.map(|v| (v * light).min(255.));
            }
            pixels.push(Color32::from_rgb(c[0] as u8, c[1] as u8, c[2] as u8));
        }
    }
    Ok(Some(Preview {
        ordered_terrain: false,
        raw_source: None,
        rivers: None,
        key: key.clone(),
        image: Arc::new(ColorImage::new([n, n], pixels)),
        display_image: None,
        landscape: None,
        heights: Arc::new(
            field
                .heights
                .iter()
                .map(|&h| {
                    (((h - 0.08) * 400. + 500.) / 1000. * 32766. + 0.5).clamp(0., 32766.) as i16
                })
                .collect(),
        ),
        resolution: n,
        metrics: Metrics::default(),
        palette: None,
        native_layers: None,
        surface: None,
        ocean: None,
    }))
}

pub struct Layout {
    pub recipe: mapper_studio::Project,
    pub solution: papyrus_terrain::placement::Solution,
    pub assets: Option<[String; 2]>,
}
pub struct LayoutTask {
    pub receiver: Receiver<Result<Option<Layout>, String>>,
    pub cancel: Arc<AtomicBool>,
}
impl LayoutTask {
    pub fn start(
        recipe: mapper_studio::Project,
        preview: &Preview,
        ctx: eframe::egui::Context,
        assets: Option<(
            Arc<mapper_studio::prefab_assets::Catalog>,
            Arc<mapper_studio::prefab_textures::Bindings>,
        )>,
        monuments: Option<Arc<mapper_studio::monument_source::Prepared>>,
    ) -> Self {
        let heights = Arc::clone(&preview.heights);
        let layers = preview.native_layers.clone();
        let resolution = preview.resolution;
        let cancel = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&cancel);
        let receiver = start_worker("mapper-placement", ctx, move |_| {
            let _verification = mapper_studio::installation::VerificationScope::default();
            let receipts = assets
                .as_ref()
                .map(|(c, b)| [c.sha256.clone(), b.sha256.clone()]);
            if mapper_studio::placement_preview::is_earthwork(&recipe.placement_engine) {
                match layers.as_ref().zip(assets.as_ref()) {
                    Some((layers, (catalog, bindings))) => {
                        let _monuments = mapper_studio::monument_source::Scope::with_prepared(
                            recipe.monument_rules.as_ref(), catalog, bindings, monuments.as_deref(),
                        );
                        mapper_studio::placement_preview::solve_native(&recipe, layers, catalog, bindings, || signal.load(Ordering::Relaxed))
                    }
                    None => Err("Installation assets must finish automatic verification before earthwork placement; check Settings".into()),
                }
            } else {
                mapper_studio::placement_preview::solve(&recipe, &heights, resolution, || signal.load(Ordering::Relaxed))
            }.map(|result| result.map(|solution| Layout { assets: (mapper_studio::placement_preview::is_earthwork(&recipe.placement_engine)).then_some(receipts).flatten(), recipe, solution }))
        });
        Self { receiver, cancel }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_thumbnail_retains_the_same_composition_inputs_as_detailed_preview() {
        use mapper_studio::native_processing::{Processed, Source};
        use papyrus_terrain::native_layers::{Layers, dimensions};

        let project = Project {
            size: 1000,
            preview_engine: mapper_studio::native_preview::VERSION.into(),
            ..Default::default()
        };
        let key = project.terrain_key();
        let (hr, sr) = dimensions(key.size).unwrap();
        let cells = sr * sr;
        let layers = Arc::new(Layers {
            size: key.size,
            terrain_seed: key.seed,
            biome_seed: key.biome_seed.unwrap(),
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![17000; hr * hr],
            biome: [vec![255; cells], vec![0; cells * 4]].concat(),
            splat: [vec![255; cells], vec![0; cells * 7]].concat(),
            topology: vec![0; cells],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        });
        let heights = Arc::new(vec![17000; 513 * 513]);
        let thumbnail = ColorImage::filled([513, 513], Color32::GREEN);
        let processed = || Processed {
            raw_source: None,
            source: Source {
                terrain: None,
                native_base: None,
                rivers: None,
                palette: None,
                key: key.clone(),
                image: thumbnail.clone(),
                heights: heights.clone(),
                layers: layers.clone(),
            },
            ocean: None,
            reused_source: true,
            ocean_ms: 0.,
            cache_status: "Test source".into(),
        };
        let cancel = AtomicBool::new(false);
        let compact = finish_native(&project, processed(), false, &cancel, Instant::now(), 0, |_| {
            panic!("A hidden queued variant requested a detailed image")
        }).unwrap().unwrap();
        let mut updates = 0;
        let detailed = finish_native(&project, processed(), true, &cancel, Instant::now(), 0, |_| {
            updates += 1;
        }).unwrap().unwrap();
        assert_eq!(updates, 1);
        assert_eq!(compact.viewport_image().as_ref(), &thumbnail);
        assert!(detailed.viewport_image().size[0] > thumbnail.size[0]);
        assert!(Arc::ptr_eq(compact.native_layers.as_ref().unwrap(), detailed.native_layers.as_ref().unwrap()));
        assert!(Arc::ptr_eq(&compact.heights, &detailed.heights));
        assert!(compact.matches_project(&project));
        cancel.store(true, Ordering::Relaxed);
        assert!(finish_native(&project, processed(), false, &cancel, Instant::now(), 0, |_| {})
            .unwrap().is_none());
    }

    #[test]
    fn headless_and_gui_landscape_generation_match() {
        let project = Project::default();
        let cancel = AtomicBool::new(false);
        let gui = build(&project.terrain_key(), &cancel, false, |_| {})
            .unwrap()
            .unwrap();
        let cli = mapper_studio::headless::generate(
            &project,
            &mapper_studio::headless::Options::default(),
            &cancel,
            |_, _| {},
        )
        .unwrap()
        .unwrap();
        assert_eq!(gui.image.as_ref(), &cli.image);
        assert_eq!(*gui.heights, cli.heights);
        assert_eq!(gui.resolution, cli.resolution);
        assert_eq!(
            mapper_studio::placement_preview::solve(&project, &gui.heights, gui.resolution, || false)
                .unwrap()
                .unwrap(),
            cli.solution
        );
    }
    #[test]
    fn repeatable_preview_and_cancel() {
        let s = AtomicBool::new(false);
        let key = Project::default().terrain_key();
        let a = build(&key, &s, false, |_| {}).unwrap().unwrap();
        let b = build(&key, &s, true, |_| {}).unwrap().unwrap();
        assert_eq!(a.image, b.image);
        assert_eq!(a.heights, b.heights);
        s.store(true, Ordering::Relaxed);
        assert!(build(&key, &s, true, |_| {}).unwrap().is_none());
    }
    #[test]
    fn stage_mailbox_keeps_one_bitmap_and_final_render_is_independent() {
        let key = Project::default().terrain_key();
        let slot = Mutex::new(None);
        let image = ColorImage::filled([129, 129], Color32::GREEN);
        store_progress(
            &slot,
            Progress {
                key: key.clone(),
                label: "Coast",
                image: Some(image.clone()),
            },
        );
        store_progress(
            &slot,
            Progress {
                key: key.clone(),
                label: "Climate",
                image: None,
            },
        );
        let update = slot.lock().unwrap().take().unwrap();
        assert_eq!(update.image.unwrap(), image);
        assert_eq!(update.label, "Climate");
        assert!(slot.lock().unwrap().is_none());
        let signal = AtomicBool::new(false);
        let mut count = 0;
        let enabled = build_with_interval(&key, &signal, true, Duration::ZERO, |p| {
            if let Some(image) = p.image {
                count += 1;
                assert_eq!(image.size, [129, 129]);
            }
        })
        .unwrap()
        .unwrap();
        let disabled = build(&key, &signal, false, |_| {}).unwrap().unwrap();
        assert_eq!(count, 3);
        assert_eq!(enabled.image, disabled.image);
        assert_eq!(enabled.heights, disabled.heights);
    }
    #[test]
    fn new_landscape_provides_feasible_default_monument_footprints() {
        let recipe = Project::default();
        let cancel = AtomicBool::new(false);
        let preview = build(&recipe.terrain_key(), &cancel, false, |_| {})
            .unwrap()
            .unwrap();
        let result = mapper_studio::placement_preview::solve(
            &recipe,
            &preview.heights,
            preview.resolution,
            || false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(result.required_missing, 0);
        assert_eq!(
            result
                .outcomes
                .iter()
                .filter(|o| o.position.is_some())
                .count(),
            3
        );
    }
    #[test]
    #[ignore = "Opt-in Release performance measurement"]
    fn benchmark_stage_preview_overhead() {
        let key = Project::default().terrain_key();
        let cancel = AtomicBool::new(false);
        let mut rows = Vec::new();
        let _ = build(&key, &cancel, false, |_| {}).unwrap();
        for iteration in 0..7 {
            for mode in if iteration % 2 == 0 {
                ["off", "normal", "all_stages"]
            } else {
                ["all_stages", "normal", "off"]
            } {
                let result = build_with_interval(
                    &key,
                    &cancel,
                    mode != "off",
                    if mode == "all_stages" {
                        Duration::ZERO
                    } else {
                        Duration::from_millis(160)
                    },
                    |_| {},
                )
                .unwrap()
                .unwrap();
                rows.push(serde_json::json!({"iteration":iteration,"mode":mode,"elapsed_ms":result.metrics.elapsed_ms,"snapshot_ms":result.metrics.snapshot_ms,"snapshots":result.metrics.snapshots}));
            }
        }
        let report = serde_json::json!({"build":"release recommended","resolution":513,"snapshot_resolution":129,"stages":3,"samples":rows});
        if let Some(path) = std::env::var_os("MAPPER_PREVIEW_BENCH_REPORT") {
            std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
        }
        println!("{report}");
    }
}
