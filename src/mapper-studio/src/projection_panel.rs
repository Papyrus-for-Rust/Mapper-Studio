//! One cancellable terrain-preview worker; immutable source grids never enter UI work.
use eframe::egui;
use mapper_studio::{
    Project, TerrainKey, composition, monument_source, prefab_assets::Catalog,
    prefab_textures::Bindings, terrain_palette::Palette,
};
use papyrus_terrain::{
    monument_composition::{self, Composed, projection::Terrain},
    native_layers::Layers,
    ocean::Ocean,
    placement::Position,
};
use std::{
    sync::{
        Arc, Condvar, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

const SETTLE: Duration = Duration::from_millis(150);
#[derive(Clone, PartialEq)]
pub(super) struct Other {
    pub id: u64,
    pub path: String,
    pub position: Position,
}
pub(super) struct Request {
    pub project: Project,
    pub layers: Arc<Layers>,
    pub ocean: Option<Arc<Ocean>>,
    pub palette: Option<Arc<Palette>>,
    pub catalog: Arc<Catalog>,
    pub bindings: Arc<Bindings>,
    pub id: u64,
    pub path: String,
    pub pose: [f32; 3],
    pub others: Vec<Other>,
}
#[derive(Clone)]
struct Key {
    terrain: TerrainKey,
    layers: Weak<Layers>,
    ocean: Option<Weak<Ocean>>,
    palette: Option<Weak<Palette>>,
    catalog: String,
    bindings: String,
    id: u64,
    path: String,
    others: Vec<Other>,
    placement: String,
    composition: Option<String>,
    rules: Option<monument_source::Recipe>,
}
fn same<T>(a: &Option<Weak<T>>, b: &Option<Weak<T>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => Weak::ptr_eq(a, b),
        _ => false,
    }
}
impl PartialEq for Key {
    fn eq(&self, b: &Self) -> bool {
        self.terrain == b.terrain
            && Weak::ptr_eq(&self.layers, &b.layers)
            && same(&self.ocean, &b.ocean)
            && same(&self.palette, &b.palette)
            && self.catalog == b.catalog
            && self.bindings == b.bindings
            && self.id == b.id
            && self.path == b.path
            && self.others == b.others
            && self.placement == b.placement
            && self.composition == b.composition
            && self.rules == b.rules
    }
}
impl Request {
    fn key(&self) -> Key {
        Key {
            terrain: self.project.terrain_key(),
            layers: Arc::downgrade(&self.layers),
            ocean: self.ocean.as_ref().map(Arc::downgrade),
            palette: self.palette.as_ref().map(Arc::downgrade),
            catalog: self.catalog.sha256.clone(),
            bindings: self.bindings.sha256.clone(),
            id: self.id,
            path: self.path.clone(),
            others: self.others.clone(),
            placement: self.project.placement_engine.clone(),
            composition: self.project.composition_engine.clone(),
            rules: self.project.monument_rules.clone(),
        }
    }
}
#[derive(Clone, Copy)]
struct Job {
    sequence: u64,
    pose: [f32; 3],
}
struct Shared {
    stop: AtomicBool,
    sequence: AtomicU64,
    job: Mutex<Option<Job>>,
    wake: Condvar,
}
struct Frame {
    moving: Option<mapper_studio::stamp_projection::MovingPatch>,
    image: egui::ColorImage,
    hit: Vec<u8>,
    size: u32,
    pose: [f32; 4],
    requested: [f32; 3],
    approximate: bool,
    warning: String,
}
enum Reply {
    Baseline(Arc<egui::ColorImage>),
    SetupError(String),
    Frame(u64, Result<Option<Frame>, String>),
}
struct Session {
    key: Key,
    shared: Arc<Shared>,
    receiver: Receiver<Reply>,
    thread: Option<JoinHandle<()>>,
}
impl Session {
    fn stop(&self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.wake.notify_all();
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}
#[derive(Default)]
pub(super) struct Panel {
    seen: bool,
    session: Option<Session>,
    wanted: Option<Key>,
    failed: Option<Key>,
    pose: Option<[f32; 3]>,
    sequence: u64,
    queued: Option<u64>,
    changed: Option<Instant>,
    texture: Option<egui::TextureHandle>,
    baseline: Option<egui::TextureHandle>,
    moving: Option<Moving>,
    accepted: Option<(u64, FrameMeta)>,
    message: String,
}
struct Moving {
    image: Arc<egui::ColorImage>,
    texture: egui::TextureHandle,
    bounds: [f32; 4],
    pose: [f32; 4],
}
struct FrameMeta {
    hit: Vec<u8>,
    resolution: usize,
    size: u32,
    pose: [f32; 4],
    requested: [f32; 3],
    approximate: bool,
}
impl Panel {
    #[cfg(test)]
    pub fn seed_test_projection(
        &mut self,
        request: Request,
        ctx: &egui::Context,
    ) -> egui::TextureId {
        let key = request.key();
        self.select(Some(key.clone()), Some(request.pose));
        let (sender, receiver) = mpsc::channel();
        self.session = Some(Session {
            key,
            shared: Arc::new(Shared {
                stop: AtomicBool::new(false),
                sequence: AtomicU64::new(self.sequence),
                job: Mutex::new(None),
                wake: Condvar::new(),
            }),
            receiver,
            thread: None,
        });
        let mut baseline = egui::ColorImage::filled([32, 32], egui::Color32::from_rgb(83, 106, 67));
        for y in 4..8 {
            for x in 4..8 {
                baseline.pixels[y * 32 + x] = egui::Color32::from_rgb(75, 125, 160);
            }
        }
        let mut image = baseline.clone();
        let mut hit = vec![0; 1024];
        for y in 8..24 {
            for x in 8..24 {
                if x < 16 || y >= 16 {
                    hit[y * 32 + x] = 1;
                    image.pixels[y * 32 + x] = egui::Color32::from_rgb(183, 167, 132);
                }
            }
        }
        let moving = mapper_studio::stamp_projection::moving_patch(
            &image,
            &hit,
            [-250., -250., 250., 250.],
            1000,
            || false,
        )
        .unwrap();
        sender.send(Reply::Baseline(Arc::new(baseline))).unwrap();
        sender
            .send(Reply::Frame(
                self.sequence,
                Ok(Some(Frame {
                    moving,
                    image,
                    hit,
                    size: 1000,
                    pose: [request.pose[0], 0., request.pose[1], request.pose[2]],
                    requested: request.pose,
                    approximate: false,
                    warning: String::new(),
                })),
            ))
            .unwrap();
        self.poll(ctx);
        self.moving.as_ref().unwrap().texture.id()
    }
    pub fn begin_frame(&mut self) {
        self.seen = false;
    }
    pub fn end_frame(&mut self, ctx: &egui::Context) {
        if !self.seen {
            self.tick(None, ctx);
        }
    }
    pub fn ready(&self) -> bool {
        self.accepted
            .as_ref()
            .is_some_and(|(n, _)| *n == self.sequence)
            && self.wanted.is_some()
    }
    pub fn texture(&self) -> Option<&egui::TextureHandle> {
        if self.ready() {
            self.texture.as_ref()
        } else {
            self.wanted.as_ref().and(self.baseline.as_ref())
        }
    }
    pub fn approximate(&self) -> Option<bool> {
        self.ready()
            .then(|| self.accepted.as_ref().unwrap().1.approximate)
    }
    pub fn message(&self) -> &str {
        &self.message
    }
    pub fn resolved_pose(&self) -> Option<[f32; 4]> {
        self.ready().then(|| self.accepted.as_ref().unwrap().1.pose)
    }
    pub fn light_pose(&self, id: u64, pose: [f32; 3]) -> [f32; 3] {
        if self.wanted.as_ref().is_some_and(|k| k.id == id)
            && let Some((_, last)) = &self.accepted
        {
            [
                pose[0] + last.pose[0] - last.requested[0],
                pose[1] + last.pose[2] - last.requested[1],
                (pose[2] + last.pose[3] - last.requested[2]).rem_euclid(360.),
            ]
        } else {
            pose
        }
    }
    fn moving_pose(&self) -> Option<(&Moving, mapper_studio::splat_preview::Geometry, [f32; 3])> {
        self.wanted.as_ref()?;
        let moving = self.moving.as_ref()?;
        let request = self.pose?;
        let pose = self.light_pose(self.wanted.as_ref()?.id, request);
        let angle = pose[2] - moving.pose[3];
        let (s, c) = angle.to_radians().sin_cos();
        let old = [
            (moving.bounds[0] + moving.bounds[2]) * 0.5,
            (moving.bounds[1] + moving.bounds[3]) * 0.5,
        ];
        let dx = old[0] - moving.pose[0];
        let dz = old[1] - moving.pose[2];
        let geometry = mapper_studio::splat_preview::Geometry {
            size: [
                moving.bounds[2] - moving.bounds[0],
                moving.bounds[3] - moving.bounds[1],
            ],
            offset: [0., 0.],
        };
        Some((
            moving,
            geometry,
            [pose[0] + c * dx - s * dz, pose[1] + s * dx + c * dz, angle],
        ))
    }
    pub fn moving_quad(&self) -> Option<(egui::TextureId, [[f32; 2]; 4])> {
        if self.ready() {
            return None;
        }
        self.proposal_quad()
    }
    /// A cropped proposal over a retained complete world, including ready frames.
    pub fn proposal_quad(&self) -> Option<(egui::TextureId, [[f32; 2]; 4])> {
        self.moving_pose().map(|(moving, geometry, pose)| {
            (
                moving.texture.id(),
                geometry.quad(pose[0], pose[1], pose[2]),
            )
        })
    }
    pub fn has_paint(&self) -> bool {
        self.ready() || self.moving_pose().is_some()
    }
    pub fn hit_test(&self, id: u64, point: [f32; 2]) -> Option<bool> {
        if self.wanted.as_ref()?.id != id {
            return None;
        }
        if !self.ready() {
            return self.moving_pose().map(|(moving, geometry, pose)| {
                mapper_studio::splat_preview::hit_test(&moving.image, geometry, point, pose)
            });
        }
        let meta = &self.accepted.as_ref()?.1;
        let n = meta.resolution;
        let u = point[0] / meta.size as f32 + 0.5;
        let v = 0.5 - point[1] / meta.size as f32;
        if !u.is_finite() || !v.is_finite() || !(0. ..=1.).contains(&u) || !(0. ..=1.).contains(&v)
        {
            return Some(false);
        }
        Some(
            meta.hit
                [((v * n as f32) as usize).min(n - 1) * n + ((u * n as f32) as usize).min(n - 1)]
                != 0,
        )
    }
    fn select(&mut self, key: Option<Key>, pose: Option<[f32; 3]>) {
        if self.wanted != key {
            if let Some(session) = &self.session {
                session.stop();
            }
            self.wanted = key.clone();
            self.failed = None;
            self.accepted = None;
            self.texture = None;
            self.baseline = None;
            self.moving = None;
            self.pose = None;
            self.message.clear();
        }
        if self.pose != pose {
            self.pose = pose;
            self.sequence = self.sequence.wrapping_add(1);
            self.queued = None;
            self.changed = Some(Instant::now());
            self.message.clear();
            if let Some(session) = &self.session {
                session
                    .shared
                    .sequence
                    .store(self.sequence, Ordering::Relaxed);
            }
        }
    }
    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(session) = &self.session {
            while let Ok(reply) = session.receiver.try_recv() {
                if Some(&session.key) != self.wanted.as_ref()
                    || session.shared.stop.load(Ordering::Relaxed)
                {
                    continue;
                }
                match reply {
                    Reply::Baseline(image) => {
                        self.baseline = Some(ctx.load_texture(
                            "projected-monument-background",
                            egui::ImageData::Color(image),
                            egui::TextureOptions::LINEAR,
                        ));
                    }
                    Reply::SetupError(e) => {
                        self.message = e;
                        self.failed = Some(session.key.clone());
                    }
                    Reply::Frame(n, Ok(Some(frame))) if n == self.sequence => {
                        let resolution = frame.image.size[0];
                        self.moving = frame.moving.map(|patch| {
                            let image = Arc::new(patch.image);
                            let texture = ctx.load_texture(
                                "moving-source-monument",
                                egui::ImageData::Color(image.clone()),
                                egui::TextureOptions::LINEAR,
                            );
                            Moving {
                                image,
                                texture,
                                bounds: patch.world_bounds,
                                pose: frame.pose,
                            }
                        });
                        if let Some(texture) = &mut self.texture {
                            texture.set(frame.image, egui::TextureOptions::LINEAR);
                        } else {
                            self.texture = Some(ctx.load_texture(
                                "projected-monument-terrain",
                                frame.image,
                                egui::TextureOptions::LINEAR,
                            ));
                        }
                        self.accepted = Some((
                            n,
                            FrameMeta {
                                hit: frame.hit,
                                resolution,
                                size: frame.size,
                                pose: frame.pose,
                                requested: frame.requested,
                                approximate: frame.approximate,
                            },
                        ));
                        self.message = frame.warning;
                    }
                    Reply::Frame(n, Err(e)) if n == self.sequence => self.message = e,
                    _ => {}
                }
            }
        }
        if self
            .session
            .as_ref()
            .is_some_and(|s| s.thread.as_ref().is_some_and(JoinHandle::is_finished))
        {
            let mut session = self.session.take().unwrap();
            if let Some(thread) = session.thread.take()
                && thread.join().is_err()
                && self.wanted.as_ref() == Some(&session.key)
            {
                self.failed = Some(session.key.clone());
                self.message = "Terrain preview worker stopped".into();
            }
        }
    }
    pub fn tick(&mut self, request: Option<Request>, ctx: &egui::Context) {
        self.seen = true;
        let invalid = request.as_ref().is_some_and(|r| {
            !r.pose.iter().all(|v| v.is_finite())
                || r.pose[0].abs() > r.project.size as f32 * 0.5
                || r.pose[1].abs() > r.project.size as f32 * 0.5
        });
        let request = request.filter(|_| !invalid);
        let key = request.as_ref().map(Request::key);
        let pose = request.as_ref().map(|r| r.pose);
        self.select(key.clone(), pose);
        self.poll(ctx);
        if invalid {
            self.message = "Requested terrain preview lies outside the map".into();
        }
        if request.is_none() {
            return;
        }
        if self.session.is_none()
            && self.failed != key
            && let Some(request) = request
        {
            let key = key.unwrap();
            self.session = Some(spawn(request, key, self.sequence, ctx.clone()));
        }
        if let Some(session) = &self.session {
            if session.shared.stop.load(Ordering::Relaxed) {
                ctx.request_repaint_after(Duration::from_millis(10));
                return;
            }
            if self.queued != Some(self.sequence) {
                let elapsed = self.changed.map_or(SETTLE, |t| t.elapsed());
                if elapsed < SETTLE {
                    ctx.request_repaint_after(SETTLE - elapsed);
                    return;
                }
                if let Some(pose) = self.pose
                    && let Ok(mut slot) = session.shared.job.lock()
                {
                    *slot = Some(Job {
                        sequence: self.sequence,
                        pose,
                    });
                    self.queued = Some(self.sequence);
                    session.shared.wake.notify_one();
                }
            }
        }
    }
}

fn spawn(request: Request, key: Key, sequence: u64, ctx: egui::Context) -> Session {
    let shared = Arc::new(Shared {
        stop: AtomicBool::new(false),
        sequence: AtomicU64::new(sequence),
        job: Mutex::new(None),
        wake: Condvar::new(),
    });
    let worker = shared.clone();
    let (sender, receiver) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        let stop = || worker.stop.load(Ordering::Relaxed);
        let data = match prepare(&request, stop) {
            Ok(Some(data)) => data,
            Ok(None) => return,
            Err(e) => {
                let _ = sender.send(Reply::SetupError(e));
                ctx.request_repaint();
                return;
            }
        };
        if !stop() {
            let _ = sender.send(Reply::Baseline(data.baseline.clone()));
            ctx.request_repaint();
        }
        loop {
            let job = {
                let Ok(mut slot) = worker.job.lock() else {
                    return;
                };
                while slot.is_none() && !stop() {
                    let Ok(next) = worker.wake.wait(slot) else {
                        return;
                    };
                    slot = next;
                }
                if stop() {
                    return;
                }
                slot.take().unwrap()
            };
            let cancelled = || stop() || worker.sequence.load(Ordering::Relaxed) != job.sequence;
            let result = build(&request, &data, job.pose, cancelled);
            if !cancelled() {
                let _ = sender.send(Reply::Frame(job.sequence, result));
                ctx.request_repaint();
            }
        }
    });
    Session {
        key,
        shared,
        receiver,
        thread: Some(thread),
    }
}
struct Prepared {
    maps: Composed,
    stamp: composition::Prepared,
    rules: Option<monument_source::Source>,
    navigation: Option<papyrus_terrain::boat_navigation::Navigation>,
    baseline: Arc<egui::ColorImage>,
}
fn prepare(request: &Request, cancelled: impl Fn() -> bool) -> Result<Option<Prepared>, String> {
    request.project.validate()?;
    request
        .project
        .check_monument_assets(&request.catalog.sha256, &request.bindings.sha256)?;
    let layers = &request.layers;
    let (hr, n) = papyrus_terrain::native_layers::dimensions(request.project.size)?;
    if layers.height_resolution != hr
        || layers.surface_resolution != n
        || request.project.installation.as_ref() != Some(&request.catalog.installation)
        || request.bindings.catalog_sha256() != request.catalog.sha256
    {
        return Err("Terrain preview installation/assets mismatch".into());
    }
    if layers.size != request.project.size
        || layers.terrain_seed != request.project.seeds[0].value
        || layers.biome_seed != request.project.seeds[1].value
    {
        return Err("Terrain preview source differs from the recipe".into());
    }
    if request.project.rivers.is_some() && request.ocean.is_none() {
        return Err("Registered river water is not ready".into());
    }
    let alpha = vec![255; n * n];
    let mut seen = std::collections::HashSet::new();
    let Some(maps) = monument_composition::compose_streamed(layers, &alpha, &cancelled, |apply| {
        for other in &request.others {
            if other.id == request.id {
                return Err("Preview baseline includes the moved monument".into());
            }
            if !seen.insert(other.id) {
                return Err("Duplicate unchanged monument identity".into());
            }
            let asset = request
                .catalog
                .entries
                .get(&other.path)
                .ok_or("Missing unchanged monument metadata")?;
            if composition::omit_terrain_modifier(&request.project, asset, &other.position)? {
                continue;
            }
            let Some(prepared) =
                composition::prepare(asset, &request.bindings, layers.size, &cancelled)?
            else {
                return Ok(false);
            };
            let p = &other.position;
            if !apply(&prepared.stamp_for_recipe(
                [p.x as f64, p.y as f64, p.z as f64],
                p.yaw as f64,
                request.project.composition_engine.as_deref(),
            ))? {
                return Ok(false);
            }
        }
        Ok(true)
    })?
    else {
        return Ok(None);
    };
    let mask = if monument_composition::water_aware(request.project.composition_engine.as_deref()) {
        let Some(mask) = papyrus_terrain::ocean::classify_sampled(
            n,
            |x, z| {
                Ok(
                    maps.layers.height[z * (hr - 1) / (n - 1) * hr + x * (hr - 1) / (n - 1)]
                        <= 16383,
                )
            },
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        Some(mask)
    } else {
        None
    };
    let water = request.ocean.as_ref().map(|o| o.local_water.as_slice());
    let Some(baseline) = mapper_studio::map_image::render_with_ocean_mask(
        &maps.layers,
        Some(&maps.alpha),
        mask.as_ref(),
        water,
        mapper_studio::map_image::DISPLAY_RESOLUTION,
        request.palette.as_deref(),
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    let asset = request
        .catalog
        .entries
        .get(&request.path)
        .ok_or("Missing selected monument metadata")?;
    let Some(stamp) = composition::prepare(asset, &request.bindings, layers.size, &cancelled)?
    else {
        return Ok(None);
    };
    let rules = request
        .project
        .monument_rules
        .as_ref()
        .filter(|_| monument_source::supported(&request.project.placement_engine))
        .map(|r| {
            monument_source::Source::cached(r, &request.catalog, &request.bindings, &cancelled)
        })
        .transpose()?;
    let navigation = if let Some(profile) = rules.as_ref().and_then(|r| r.coastal_profile.as_ref())
    {
        let Some(nav) = profile.navigation(&request.layers, &cancelled)? else {
            return Ok(None);
        };
        Some(nav)
    } else {
        None
    };
    Ok((!cancelled()).then_some(Prepared {
        maps,
        stamp,
        rules,
        navigation,
        baseline: Arc::new(baseline),
    }))
}
struct Resolved {
    pose: [f32; 4],
    warning: String,
}
fn resolve_pose(
    request: &Request,
    data: &Prepared,
    pose: [f32; 3],
    cancelled: impl Fn() -> bool,
) -> Result<Option<Resolved>, String> {
    // A preference can be infeasible and still have a useful hypothetical preview.
    // These warnings never change the generation recipe or certify acceptance.
    let mut warning = String::new();
    let [mut x, mut z, mut yaw] = pose;
    let half = request.layers.size as f32 * 0.5;
    if !pose.iter().all(|v| v.is_finite()) || x.abs() > half || z.abs() > half {
        return Err("Requested terrain preview lies outside the map".into());
    }
    let angle = |yaw| {
        request
            .project
            .monument_rules
            .as_ref()
            .map_or(yaw, |r| r.placement_yaw(yaw))
    };
    yaw = angle(yaw);
    let raw = papyrus_terrain::placement::Terrain {
        heights: &request.layers.height,
        resolution: request.layers.height_resolution,
        size: request.layers.size as usize,
    };
    let mut y = raw.height_at(x, z);
    if let Some(source) = &data.rules {
        if let Some(stages) = &source.stage_source {
            match stages.evaluate(
                &request.layers,
                &request.path,
                request.project.seeds[4].value,
                request.id,
                [x, z],
            )? {
                papyrus_terrain::monument_stage::Check::Accepted { .. } => {}
                papyrus_terrain::monument_stage::Check::FilterRejected { .. } => {
                    warning = "Authored monument stage filter failed".into()
                }
                papyrus_terrain::monument_stage::Check::Skipped(reason) => warning = reason.into(),
            }
        }
        let prefab = source
            .prefabs
            .get(&request.path)
            .ok_or("Missing placement rules for terrain preview")?;
        let aligned = mapper_studio::monument_alignment::program(
            request.project.monument_rules.as_ref(),
            prefab,
        );
        let coastal = prefab
            .placement_attributes
            .as_ref()
            .filter(|a| mapper_studio::coastal_placement::swims(a))
            .zip(source.coastal_profile.as_ref());
        if let Some((attributes, profile)) = coastal {
            let filter = profile
                .filter(&request.path)
                .ok_or("Coastal preview requires its source placement family")?;
            if papyrus_terrain::spawn::sample_factor(
                &request.layers,
                filter,
                x / request.layers.size as f32 + 0.5,
                z / request.layers.size as f32 + 0.5,
            )
            .unwrap_or(0.)
                <= 0.
            {
                warning = "Authored coastal stage filter failed".into();
            }
            let adapt = if request.project.monument_rules.as_ref().is_some_and(|r| {
                r.coastal_normals.as_deref() == Some(papyrus_terrain::terrain_normal::VERSION)
            }) {
                mapper_studio::coastal_placement::pose_source_normal
            } else {
                mapper_studio::coastal_placement::pose
            };
            let (position, rotation) = adapt(
                &request.layers,
                attributes,
                [x, y, z],
                yaw,
                profile.sea_level,
            )?;
            [x, y, z] = position;
            yaw = angle(rotation);
        }
        let mut rules = prefab.rules.clone();
        if coastal.is_some() || aligned.is_some() {
            rules.unsupported.retain(|s| s != mapper_studio::monument_alignment::REQUIREMENT);
        }
        if let Some((_, profile)) = coastal {
            rules.min_corners = profile.minimum_tier_corners;
        }
        if aligned.is_some() {
            yaw = angle(mapper_studio::monument_alignment::yaw(&request.layers, x, z)?);
        }
        let bounds =
            mapper_studio::placement_preview::bounds_for_recipe(&request.project, &request.catalog.entries[&request.path])?
                .0;
        if bounds
            .iter()
            .any(|b| !b.at([x, z], yaw).inside_world(request.layers.size as f32))
        {
            warning = "Authored footprint reaches outside world bounds".into();
        }
        let allows_submerged = coastal.is_some()
            || source
                .stage_source
                .as_ref()
                .is_some_and(|stages| stages.requires_ocean(&request.path));
        if !allows_submerged && y <= 4. {
            warning = "Monument preference is low or submerged".into();
        }
        if !rules.unsupported.is_empty() {
            return Err("Unsupported required monument attributes".into());
        }
        let Some(mut evaluator) = papyrus_terrain::monument_eligibility::Evaluator::new(
            &request.layers,
            &rules,
            bounds[0],
            &cancelled,
        )?
        else {
            return Ok(None);
        };
        let filter = source
            .stage_source
            .as_ref()
            .and_then(|s| s.anchor_filter(&request.path));
        if let Some(filter) = filter {
            evaluator = evaluator.with_stage_filter(filter)?;
        }
        match evaluator.evaluate([x, y, z], yaw, &mut 0, 1_000_000, &cancelled)? {
            None => return Ok(None),
            Some(papyrus_terrain::monument_eligibility::Check::Rejected(reason)) => {
                warning = reason.into();
            }
            Some(papyrus_terrain::monument_eligibility::Check::Budget) => {
                warning = "Terrain preview fitting budget reached".into();
            }
            Some(papyrus_terrain::monument_eligibility::Check::Accepted(p)) => [x, y, z] = p,
        }
        if let Some(attributes) = aligned {
            use papyrus_terrain::monument_eligibility::Check;
            match mapper_studio::monument_alignment::check_terrain(
                &request.layers,
                attributes,
                filter.ok_or("Aligned monument requires an authored stage filter")?,
                [x, y, z],
                yaw,
                &mut 0,
                1_000_000,
                &cancelled,
            )? {
                None => return Ok(None),
                Some(Check::Rejected(reason)) => warning = reason.into(),
                Some(Check::Budget) => warning = "Terrain preview fitting budget reached".into(),
                Some(Check::Accepted(_)) => {}
            }
        }
        if let Some((attributes, profile)) = coastal
            && let Err(reason) = mapper_studio::coastal_placement::checks(
                &request.layers,
                attributes,
                [x, y, z],
                yaw,
                profile,
                profile
                    .filter(&request.path)
                    .ok_or("Missing coastal filter")?,
                data.navigation
                    .as_ref()
                    .ok_or("Missing coastal preview navigation")?,
            )
        {
            warning = reason;
        }
    }
    Ok((!cancelled()).then_some(Resolved {
        pose: [x, y, z, yaw],
        warning,
    }))
}
fn build(
    request: &Request,
    data: &Prepared,
    pose: [f32; 3],
    cancelled: impl Fn() -> bool,
) -> Result<Option<Frame>, String> {
    let Some(resolved) = resolve_pose(request, data, pose, &cancelled)? else {
        return Ok(None);
    };
    let p = resolved.pose;
    let Some(terrain) = Terrain::new(&data.maps.layers, &data.maps.alpha, &cancelled)? else {
        return Ok(None);
    };
    let stamp = data.stamp.stamp_for_recipe(
        [p[0] as f64, p[1] as f64, p[2] as f64],
        p[3] as f64,
        request.project.composition_engine.as_deref(),
    );
    let projected = terrain.projected(&stamp)?;
    let Some(bounds) = projected.world_bounds() else {
        return Err("Terrain preview is outside the map".into());
    };
    let Some(patch) = mapper_studio::stamp_projection::render_viewport(
        &projected,
        mapper_studio::stamp_projection::Settings {
            maximum_dimension: mapper_studio::map_image::DISPLAY_RESOLUTION,
            palette: request.palette.as_deref(),
            connected_ocean: monument_composition::water_aware(
                request.project.composition_engine.as_deref(),
            ),
            local_water: request.ocean.as_ref().map(|o| o.local_water.as_slice()),
        },
        &cancelled,
    )?
    else {
        return Ok(None);
    };
    let size = request.layers.size;
    let Some(hit) = visible_hit(&patch.image, &data.baseline, bounds, size, &cancelled)? else {
        return Ok(None);
    };
    let moving =
        mapper_studio::stamp_projection::moving_patch(&patch.image, &hit, bounds, size, &cancelled)?;
    Ok((!cancelled()).then_some(Frame {
        moving,
        image: patch.image,
        hit,
        size,
        pose: p,
        requested: pose,
        approximate: patch.approximate_colors,
        warning: resolved.warning,
    }))
}
fn visible_hit(
    image: &egui::ColorImage,
    baseline: &egui::ColorImage,
    bounds: [f32; 4],
    size: u32,
    cancelled: impl Fn() -> bool,
) -> Result<Option<Vec<u8>>, String> {
    let n = image.size[0];
    if n == 0
        || image.size[1] != n
        || baseline.size != image.size
        || image.pixels.len() != n * n
        || baseline.pixels.len() != n * n
    {
        return Err("Terrain preview image dimensions differ".into());
    }
    let mut hit = vec![0; n * n];
    for z in 0..n {
        if cancelled() {
            return Ok(None);
        }
        let wz = (0.5 - (z as f32 + 0.5) / n as f32) * size as f32;
        for x in 0..n {
            let wx = ((x as f32 + 0.5) / n as f32 - 0.5) * size as f32;
            if wx < bounds[0] || wx > bounds[2] || wz < bounds[1] || wz > bounds[3] {
                continue;
            }
            let i = z * n + x;
            let a = image.pixels[i].to_array();
            let b = baseline.pixels[i].to_array();
            hit[i] = u8::from((0..3).any(|c| a[c].abs_diff(b[c]) >= 2));
        }
    }
    Ok((!cancelled()).then_some(hit))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn layers() -> Arc<Layers> {
        Arc::new(Layers {
            size: 1000,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: 2,
            surface_resolution: 2,
            height: vec![18000; 4],
            splat: vec![0; 32],
            biome: vec![0; 20],
            topology: vec![0; 4],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        })
    }
    fn key(layers: &Arc<Layers>) -> Key {
        Key {
            terrain: Project::default().terrain_key(),
            layers: Arc::downgrade(layers),
            ocean: None,
            palette: None,
            catalog: "a".repeat(64),
            bindings: "b".repeat(64),
            id: 1,
            path: "test.prefab".into(),
            others: Vec::new(),
            placement: "test".into(),
            composition: None,
            rules: None,
        }
    }
    fn frame() -> Frame {
        Frame {
            moving: None,
            image: egui::ColorImage::filled([2, 2], egui::Color32::GREEN),
            hit: vec![0, 1, 0, 0],
            size: 1000,
            pose: [10., 17., 20., 47.],
            requested: [0., 0., 37.],
            approximate: false,
            warning: "Authored terrain filter failed".into(),
        }
    }
    fn session(key: Key) -> (Session, mpsc::Sender<Reply>, mpsc::Sender<()>) {
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            sequence: AtomicU64::new(0),
            job: Mutex::new(None),
            wake: Condvar::new(),
        });
        let (sender, receiver) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let _ = wait.recv_timeout(Duration::from_secs(2));
        });
        (
            Session {
                key,
                shared,
                receiver,
                thread: Some(thread),
            },
            sender,
            release,
        )
    }
    #[test]
    fn stale_pose_and_source_replies_never_publish_and_source_retirement_does_not_block_ui() {
        let ctx = egui::Context::default();
        let layers = layers();
        let key = key(&layers);
        let mut panel = Panel::default();
        panel.select(Some(key.clone()), Some([0., 0., 37.]));
        let old = panel.sequence;
        let (s, sender, release) = session(key.clone());
        let shared = s.shared.clone();
        panel.session = Some(s);
        panel.select(Some(key.clone()), Some([5., 6., 52.]));
        assert_eq!(shared.sequence.load(Ordering::Relaxed), panel.sequence);
        sender.send(Reply::Frame(old, Ok(Some(frame())))).unwrap();
        panel.poll(&ctx);
        assert!(!panel.ready());
        assert!(panel.texture().is_none());
        let mut image = frame();
        image.requested = [5., 6., 52.];
        sender
            .send(Reply::Frame(panel.sequence, Ok(Some(image))))
            .unwrap();
        panel.poll(&ctx);
        assert!(panel.ready());
        assert_eq!(panel.message(), "Authored terrain filter failed");
        let handle = panel.texture().unwrap().id();
        assert_eq!(Arc::strong_count(&layers), 1);
        let mut delta = ctx.tex_manager().write().take_delta();
        assert!(delta.set.iter().any(|(id, _)| *id == handle));
        delta.clear();
        let mut replacement = key.clone();
        replacement.bindings = "c".repeat(64);
        let started = Instant::now();
        panel.select(Some(replacement), Some([5., 6., 52.]));
        panel.poll(&ctx);
        assert!(started.elapsed() < Duration::from_millis(200));
        assert!(!panel.ready());
        assert!(panel.texture().is_none());
        assert!(panel.message().is_empty());
        assert!(shared.stop.load(Ordering::Relaxed));
        assert!(
            !panel
                .session
                .as_ref()
                .unwrap()
                .thread
                .as_ref()
                .unwrap()
                .is_finished()
        );
        sender
            .send(Reply::Frame(panel.sequence, Ok(Some(frame()))))
            .unwrap();
        panel.poll(&ctx);
        assert!(!panel.ready());
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while panel.session.is_some() && Instant::now() < deadline {
            panel.poll(&ctx);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(panel.session.is_none());
    }
    #[test]
    fn visible_change_picking_and_light_pose_offset_survive_pose_updates_and_clear_on_deselection()
    {
        let ctx = egui::Context::default();
        let layers = layers();
        let key = key(&layers);
        let mut panel = Panel::default();
        panel.select(Some(key.clone()), Some([0., 0., 37.]));
        let (s, sender, release) = session(key.clone());
        panel.session = Some(s);
        sender
            .send(Reply::Frame(panel.sequence, Ok(Some(frame()))))
            .unwrap();
        panel.poll(&ctx);
        assert_eq!(panel.hit_test(1, [250., 250.]), Some(true));
        assert_eq!(panel.hit_test(1, [-250., 250.]), Some(false));
        assert_eq!(panel.hit_test(1, [501., 0.]), Some(false));
        assert_eq!(panel.hit_test(2, [250., 250.]), None);
        assert_eq!(panel.light_pose(1, [0., 0., 37.]), [10., 20., 47.]);
        let shared = panel.session.as_ref().unwrap().shared.clone();
        panel.select(Some(key), Some([5., 6., 52.]));
        assert!(Arc::ptr_eq(
            &shared,
            &panel.session.as_ref().unwrap().shared
        ));
        assert!(!panel.ready());
        assert_eq!(panel.light_pose(1, [5., 6., 52.]), [15., 26., 62.]);
        panel.begin_frame();
        panel.end_frame(&ctx);
        assert!(!panel.ready());
        assert_eq!(panel.hit_test(1, [250., 250.]), None);
        assert_eq!(panel.light_pose(1, [5., 6., 52.]), [5., 6., 52.]);
        assert!(shared.stop.load(Ordering::Relaxed));
        release.send(()).unwrap();
    }
    #[test]
    fn pending_source_paint_keeps_background_but_never_survives_source_replacement() {
        let ctx = egui::Context::default();
        let layers = layers();
        let key = key(&layers);
        let mut panel = Panel::default();
        panel.select(Some(key.clone()), Some([0., 0., 37.]));
        let (s, sender, release) = session(key.clone());
        panel.session = Some(s);
        let baseline = Arc::new(egui::ColorImage::filled([2, 2], egui::Color32::BLUE));
        sender.send(Reply::Baseline(baseline.clone())).unwrap();
        let mut valid = frame();
        valid.moving = Some(mapper_studio::stamp_projection::MovingPatch {
            image: egui::ColorImage::filled([2, 2], egui::Color32::GREEN),
            world_bounds: [-100., -100., 100., 100.],
        });
        sender
            .send(Reply::Frame(panel.sequence, Ok(Some(valid))))
            .unwrap();
        panel.poll(&ctx);
        assert!(panel.ready());
        assert!(panel.moving_quad().is_none());
        let background = panel.baseline.as_ref().unwrap().id();
        panel.select(Some(key.clone()), Some([15., 20., 92.]));
        assert!(!panel.ready());
        assert!(panel.has_paint());
        assert!(panel.moving_quad().is_some());
        assert_eq!(panel.texture().unwrap().id(), background);
        let mut replacement = key;
        replacement.bindings = "c".repeat(64);
        panel.select(Some(replacement), Some([15., 20., 92.]));
        sender.send(Reply::Baseline(baseline)).unwrap();
        panel.poll(&ctx);
        assert!(panel.texture().is_none());
        assert!(panel.moving_quad().is_none());
        assert!(!panel.has_paint());
        release.send(()).unwrap();
        let mut delta = ctx.tex_manager().write().take_delta();
        delta.clear();
    }
    #[test]
    fn interaction_mask_ignores_remote_water_changes_and_single_byte_rounding_noise() {
        let baseline = egui::ColorImage::filled([16, 16], egui::Color32::from_rgb(100, 120, 140));
        let mut image = baseline.clone();
        image.pixels[17] = egui::Color32::BLUE;
        image.pixels[7 * 16 + 8] = egui::Color32::RED;
        image.pixels[8 * 16 + 7] = egui::Color32::from_rgb(101, 120, 140);
        let hit = visible_hit(&image, &baseline, [-100., -100., 100., 100.], 1000, || {
            false
        })
        .unwrap()
        .unwrap();
        assert_eq!(hit.iter().filter(|&&v| v != 0).count(), 1);
        assert_eq!(hit[7 * 16 + 8], 1);
        assert_eq!(hit[17], 0);
        assert_eq!(hit[8 * 16 + 7], 0);
        assert!(
            visible_hit(&image, &baseline, [-100., -100., 100., 100.], 1000, || true)
                .unwrap()
                .is_none()
        );
        assert!(
            visible_hit(
                &image,
                &egui::ColorImage::filled([2, 2], egui::Color32::WHITE),
                [-100., -100., 100., 100.],
                1000,
                || false
            )
            .is_err()
        );
    }
}
