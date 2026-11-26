use eframe::egui::{self, Color32, Rect, Stroke};
use mapper_studio::{Project, installation::Installation, prefab_assets::Catalog};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
};

struct Task {
    cancel: Arc<AtomicBool>,
    receiver: Receiver<Result<Prepared, String>>,
}

struct Prepared {
    catalog: Arc<Catalog>,
    footprints: Arc<HashMap<String, Footprints>>,
}

fn is_surface_prefab(path: &str) -> bool {
    !path.contains("/monument/cave/")
        && !path.contains("/underwater_lab/")
        && !path.contains("/autospawn/tunnel/")
        && !path.contains("/autospawn/tunnel-base/")
        && mapper_studio::monument_icons::rock_marker(path).is_none()
}

fn surface_area(path: &str, geometry: &mapper_studio::no_build::Geometry) -> Option<f32> {
    use mapper_studio::no_build::Geometry;
    const MIN_WIDTH: f32 = 40.;
    // A surface rail portal's elevated sphere is authored around its exit.
    // Its wider, shallow box follows the underground tracks at the prefab root.
    if path.contains("/autospawn/tunnel-upwards/")
        && !matches!(geometry, Geometry::Sphere { center, .. } if center[1] > 0.)
    {
        return None;
    }
    let (width, top, area) = match geometry {
        Geometry::Sphere { center, radius } => (
            2. * radius, center[1] + radius, std::f32::consts::PI * radius * radius,
        ),
        Geometry::Capsule { start, end, radius } => {
            let diameter = 2. * radius;
            let length = (end[0] - start[0]).hypot(end[2] - start[2]);
            (diameter, start[1].max(end[1]) + radius,
                length * diameter + std::f32::consts::PI * radius * radius)
        }
        Geometry::Box { center, half_axes, .. } => {
            // A diagonal wall can have a large bounding rectangle. Measure
            // its cross-section perpendicular to the projected box edges.
            let mut width = f32::INFINITY;
            let mut area = 0.;
            for (i, &[x, _, z]) in half_axes.iter().enumerate() {
                let length = x.hypot(z);
                if length > 1e-4 {
                    let across = 2. * half_axes.iter()
                        .map(|h| (x * h[2] - z * h[0]).abs()).sum::<f32>() / length;
                    width = width.min(across);
                    // A projected box is the sum of the three projected
                    // edges. Sum the parallelogram areas once per edge pair.
                    area += 4. * half_axes[i + 1..].iter()
                        .map(|h| (x * h[2] - z * h[0]).abs()).sum::<f32>();
                }
            }
            let half_height = half_axes.iter().map(|h| h[1].abs()).sum::<f32>();
            (width, center[1] + half_height, area)
        }
        Geometry::Mesh { vertices, .. } => {
            let mut bounds = Rect::NOTHING;
            let mut top = f32::NEG_INFINITY;
            for &[x, y, z] in vertices {
                bounds.extend_with(egui::pos2(x, z));
                top = top.max(y);
            }
            (bounds.width().min(bounds.height()), top, bounds.area())
        }
    };
    (top > 0. && width >= MIN_WIDTH && area > 0.).then_some(area)
}

impl Prepared {
    fn new(catalog: Arc<Catalog>, cancelled: impl Fn() -> bool) -> Result<Self, String> {
        let mut footprints = HashMap::new();
        for (path, asset) in &catalog.entries {
            if cancelled() {
                return Err("No-build preparation cancelled".into());
            }
            let Some(report) = &asset.no_build_geometry else {
                continue;
            };
            if !is_surface_prefab(path) {
                continue;
            }
            let harbor = path.ends_with("/monument/harbor/harbor_1.prefab")
                || path.ends_with("/monument/harbor/harbor_2.prefab");
            let mut perimeters = Vec::new();
            for volume in report.volumes.iter().filter(|volume| volume.active) {
                if cancelled() {
                    return Err("No-build preparation cancelled".into());
                }
                if let Some(area) = surface_area(path, &volume.geometry) {
                    perimeters.push((area, &volume.geometry));
                }
            }
            perimeters.sort_by(|(a, _), (b, _)| b.total_cmp(a));
            let primary = perimeters.first().into_iter().copied();
            let harbor_zones = perimeters.iter().skip(1).copied().filter(|(_, geometry)| {
                matches!(geometry, mapper_studio::no_build::Geometry::Sphere { .. })
            }).take(if harbor { 2 } else { 0 });
            let polygons = primary.chain(harbor_zones)
                .flat_map(|(_, geometry)| geometry.footprints()).collect();
            footprints.insert(path.clone(), Footprints::new(polygons));
        }
        if cancelled() {
            return Err("No-build preparation cancelled".into());
        }
        Ok(Self { catalog, footprints: Arc::new(footprints) })
    }
}
struct Footprints {
    polygons: Vec<Vec<[f32; 2]>>,
    bounds: Rect,
}

impl Footprints {
    fn new(polygons: Vec<Vec<[f32; 2]>>) -> Self {
        let mut bounds = Rect::NOTHING;
        for &[x, z] in polygons.iter().flatten() {
            bounds.extend_with(egui::pos2(x, z));
        }
        Self { polygons, bounds }
    }

    fn projected_bounds(&self, project: impl Fn([f32; 2]) -> egui::Pos2) -> Rect {
        let Rect { min, max } = self.bounds;
        Rect::from_points(&[
            project([min.x, min.y]),
            project([max.x, min.y]),
            project([max.x, max.y]),
            project([min.x, max.y]),
        ])
    }
}
pub struct Paint<'a> {pub map:Rect,pub viewport:Rect,pub size:f32,pub project:&'a Project,pub placements:&'a [(u64,papyrus_terrain::placement::Position)],pub requested:Option<(u64,[f32;3])>}
impl Drop for Task {fn drop(&mut self){self.cancel.store(true,Ordering::Relaxed);}}
#[derive(Default)]
pub struct Panel {
    source:Option<Installation>,catalog:Option<Arc<Catalog>>,task:Option<Task>,attempted:bool,
    tunnel_markers: Option<mapper_studio::client_preparation::TunnelMarkerOffsets>,
    pub message:String,
    footprints:Arc<HashMap<String,Footprints>>,
}
#[derive(serde::Serialize,serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Snapshot {
    sha256:String,
    entries:std::collections::BTreeMap<String,(Option<mapper_studio::no_build::Report>,Option<String>)>,
}
impl Panel {
    pub(super) fn snapshot(
        &self,
        monuments: &[super::map_monuments::Monument],
    ) -> Option<Snapshot> {
        let catalog = self.catalog.as_ref()?;
        Some(Snapshot {
            sha256: catalog.sha256.clone(),
            entries: monuments
                .iter()
                .filter_map(|monument| {
                    let path = monument.prefab.as_ref()?;
                    let asset = catalog.entries.get(path)?;
                    Some((
                        path.clone(),
                        (asset.no_build_geometry.clone(), asset.display_name.clone()),
                    ))
                })
                .collect(),
        })
    }
    pub(super) fn from_snapshot(
        snapshot: Option<Snapshot>,
        project: &Project,
    ) -> Result<Self, String> {
        let Some(snapshot) = snapshot else {
            return Ok(Self::default());
        };
        let installation = project
            .installation
            .clone()
            .ok_or("Saved restrictions require an installation identity")?;
        if snapshot.entries.len() > 65536
            || snapshot.sha256.len() != 64
            || !snapshot
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("Invalid saved restriction metadata".into());
        }
        let mut entries = std::collections::HashMap::new();
        for (path, (report, name)) in snapshot.entries {
            if path.len() > 512
                || !path.starts_with("assets/")
                || !path.ends_with(".prefab")
                || path.chars().any(char::is_control)
                || path.split('/').any(|part| part == "..")
                || name
                    .as_ref()
                    .is_some_and(|name| name.len() > 1024 || name.chars().any(char::is_control))
            {
                return Err("Invalid saved restriction prefab".into());
            }
            if let Some(report) = &report {
                report.validate()?;
            }
            entries.insert(
                path,
                mapper_studio::prefab_assets::Asset {
                    priority: None,
                    bounds: None,
                    yaw: None,
                    modifier_size: None,
                    modifier_offset: None,
                    texture_guids: Default::default(),
                    splat_mask: None,
                    modifier: None,
                    modifier_absent: false,
                    water_geometry: None,
                    no_build_geometry: report,
                    display_name: name,
                },
            );
        }
        let mut panel = Self {
            source: Some(installation.clone()),
            attempted: true,
            ..Default::default()
        };
        let catalog = Arc::new(Catalog {
            sha256: snapshot.sha256,
            installation,
            entries,
            sources: Vec::new(),
        });
        panel.publish(Prepared::new(catalog, || false)?);
        Ok(panel)
    }
    pub fn for_export(&self) -> Self {
        Self {
            source: self.source.clone(),
            catalog: self.catalog.clone(),
            footprints: self.footprints.clone(),
            message: self.message.clone(),
            attempted: true,
            ..Default::default()
        }
    }
    pub fn incomplete_count(
        &self,
        project: &Project,
        monuments: &[super::map_monuments::Monument],
    ) -> usize {
        let catalog = self.catalog_for(project);
        monuments
            .iter()
            .filter(|m| m.prefab.as_deref().is_none_or(is_surface_prefab))
            .filter(|m| {
                m.restriction_pose().is_none() || m.prefab
                    .as_ref()
                    .and_then(|path| {
                        catalog
                            .as_ref()?
                            .entries
                            .get(path)?
                            .no_build_geometry
                            .as_ref()
                    })
                    .is_none_or(|r| !r.complete)
            })
            .count()
    }
    pub fn export_bytes(&self, monuments: &[super::map_monuments::Monument]) -> usize {
        monuments
            .iter()
            .filter(|m| m.restriction_pose().is_some())
            .filter_map(|m| self.footprints.get(m.prefab.as_ref()?))
            .flat_map(|footprints| &footprints.polygons)
            .map(|p| p.len().saturating_mul(192).saturating_add(128))
            .sum()
    }
    pub fn extent(&self, monuments: &[super::map_monuments::Monument]) -> f32 {
        let mut extent = 0_f32;
        for m in monuments {
            let Some([x, z, yaw]) = m.restriction_pose() else { continue };
            let (sin, cos) = yaw.to_radians().sin_cos();
            if let Some(footprints) = m.prefab.as_ref().and_then(|p| self.footprints.get(p)) {
                for p in footprints.polygons.iter().flatten() {
                    extent = extent
                        .max((x + cos * p[0] - sin * p[1]).abs())
                        .max((z + sin * p[0] + cos * p[1]).abs());
                }
            }
        }
        extent
    }
    #[cfg(test)]
    pub(super) fn from_catalog(catalog:Option<Arc<Catalog>>) -> Self {
        let mut panel=Self::default();
        if let Some(catalog)=catalog.filter(|c|Self::has_reports(c)) {panel.seed(catalog);}
        panel
    }
    fn has_reports(catalog: &Catalog) -> bool {
        !catalog.entries.is_empty()
            && catalog.entries.values().all(|asset| {
                asset.no_build_geometry.as_ref()
                    .is_some_and(|report| report.version == mapper_studio::no_build::VERSION)
            })
    }
    pub(super) fn catalog_for(&self, project: &Project) -> Option<Arc<Catalog>> {
        self.catalog.as_ref()
            .filter(|catalog| project.installation.as_ref() == Some(&catalog.installation))
            .cloned()
    }
    pub(super) fn shows_map_text(&self, path: Option<&str>) -> bool {
        if path.and_then(mapper_studio::monument_icons::rock_marker).is_some() {
            return false;
        }
        self.catalog.as_ref()
            .and_then(|catalog| catalog.entries.get(path?))
            .and_then(|asset| asset.no_build_geometry.as_ref())
            .and_then(|report| report.should_display_on_map)
            .unwrap_or(true)
    }
    pub(super) fn sync(
        &mut self,
        project: &Project,
        source: &Self,
        monuments: &mut [super::map_monuments::Monument],
    ) {
        if let Some(catalog) = source.catalog_for(project)
            && self.catalog.as_ref().is_none_or(|current| !Arc::ptr_eq(current, &catalog)) {
            self.source = Some(catalog.installation.clone());
            self.catalog = Some(catalog);
            self.footprints = source.footprints.clone();
            self.message = source.message.clone();
            if let (Some(markers), Some(assets)) = (&source.tunnel_markers, &project.monument_assets)
                && markers.catalog_sha256 == assets.catalog_sha256
            {
                for monument in monuments {
                    if let Some(offset) = monument.prefab.as_ref().and_then(|p| markers.offsets.get(p)) {
                        monument.restore_tunnel_origin(*offset);
                    }
                }
            }
        }
    }
    #[cfg(test)]
    pub(super) fn seed(&mut self,catalog:Arc<Catalog>) {
        self.source=Some(catalog.installation.clone());
        self.publish(Prepared::new(catalog, || false).unwrap());
    }
    pub(super) fn set_tunnel_markers(
        &mut self,
        project: &Project,
        markers: Option<mapper_studio::client_preparation::TunnelMarkerOffsets>,
    ) {
        self.bind_source(project);
        self.tunnel_markers = markers;
    }

    pub(super) fn pending_for(&self, project: &Project) -> bool {
        self.task.is_some() && self.catalog.is_none() && self.source == project.installation
    }

    fn bind_source(&mut self, project: &Project) {
        if self.source != project.installation {
            *self = Self { source: project.installation.clone(), ..Self::default() };
        }
    }
    fn publish(&mut self, prepared: Prepared) {
        self.message = if prepared.footprints.is_empty() {
            "Restriction volumes unavailable for this installation.".into()
        } else {
            String::new()
        };
        self.footprints = prepared.footprints;
        self.catalog = Some(prepared.catalog);
    }

    pub fn tick(
        &mut self,
        enabled: bool,
        project: &Project,
        root: Option<PathBuf>,
        catalog: Option<Arc<Catalog>>,
        ctx: &egui::Context,
    ) -> bool {
        self.bind_source(project);
        if !enabled {
            self.task = None;
            if self.catalog.is_none() {
                self.attempted = false;
                self.message.clear();
            }
            return false;
        }
        let had_catalog = self.catalog.is_some();
        if let Some(task) = &self.task {
            let result = match task.receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => Some(Err("Restriction worker stopped".into())),
            };
            if let Some(result) = result {
                let cancelled = task.cancel.load(Ordering::Relaxed);
                self.task = None;
                if cancelled {
                    self.message = "No-build preparation cancelled".into();
                } else {
                    match result {
                        Ok(prepared) => self.publish(prepared),
                        Err(error) => self.message = error,
                    }
                }
            }
        }
        if self.catalog.is_none() && !self.attempted && self.task.is_none() {
            let installation = root.zip(self.source.clone());
            if installation.is_some() || catalog.as_ref().is_some_and(|c| Self::has_reports(c)) {
                self.attempted = true;
                let cancel = Arc::new(AtomicBool::new(false));
                let signal = cancel.clone();
                let (sender, receiver) = mpsc::channel();
                let context = ctx.clone();
                std::thread::spawn(move || {
                    let _loading = mapper_studio::loading_progress::begin("Building restrictions", Some(signal.clone()));
                    let cancelled = || signal.load(Ordering::Relaxed);
                    let result = prepare_catalog(installation, catalog, cancelled)
                        .and_then(|catalog| Prepared::new(catalog, cancelled));
                    let _ = sender.send(result);
                    context.request_repaint();
                });
                self.task = Some(Task { cancel, receiver });
            }
        }
        if self.task.is_some() {
            self.message = "Loading restriction volumesâ€¦".into();
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        !had_catalog && self.catalog.is_some()
    }
    pub fn paint(&self,painter:&egui::Painter,input:Paint<'_>)->(usize,usize) {
        let Paint {map,viewport,size,project,placements,requested}=input;
        let poses = placements.iter()
            .filter(|(id, _)| requested.is_none_or(|r| r.0 != *id))
            .map(|(id, p)| (*id, [p.x, p.z, p.yaw]))
            .chain(requested);
        self.paint_poses(painter,map,viewport,size,poses.map(|(id,pose)|
            (project.targets.iter().find(|t|t.instance_id==id).and_then(|t|t.prefab_path.as_deref()),pose)),4096)
    }
    pub(super) fn paint_monuments(&self,painter:&egui::Painter,map:Rect,viewport:Rect,size:f32,monuments:&[super::map_monuments::Monument])->(usize,usize) {
        self.paint_poses(painter,map,viewport,size,monuments.iter().filter(|m| !m.is_scenery_marker()).map(restriction_marker_pose),4096)
    }
    pub fn paint_export(&self,painter:&egui::Painter,map:Rect,viewport:Rect,size:f32,monuments:&[super::map_monuments::Monument])->(usize,usize) {
        self.paint_poses(painter,map,viewport,size,monuments.iter().filter(|m| !m.is_scenery_marker()).map(restriction_marker_pose),usize::MAX)
    }
    fn paint_poses<'a>(&self,painter:&egui::Painter,map:Rect,viewport:Rect,size:f32,poses:impl Iterator<Item=(Option<&'a str>,[f32;3])>,maximum:usize)->(usize,usize) {
        let poses = poses.filter(|(path, _)| path.is_none_or(is_surface_prefab));
        let Some(catalog)=&self.catalog else{return (0,poses.count());};
        let painter=painter.with_clip_rect(viewport);let mut drawn=0;let mut partial=0;
        for (path,pose) in poses {
            let Some(path)=path else{partial+=1;continue;};
            let report=catalog.entries.get(path).and_then(|a|a.no_build_geometry.as_ref());
            if report.is_none_or(|r|!r.complete){partial+=1;}
            let Some(footprints) = self.footprints.get(path) else { continue; };
            if footprints.polygons.is_empty() { continue; }
            if drawn >= maximum { return (drawn, partial + 1); }
            let (sin, cos) = pose[2].to_radians().sin_cos();
            let project = |[px, pz]: [f32; 2]| {
                let x = pose[0] + cos * px - sin * pz;
                let z = pose[1] + sin * px + cos * pz;
                egui::pos2(
                    map.left() + (x / size + 0.5) * map.width(),
                    map.top() + (0.5 - z / size) * map.height(),
                )
            };
            // An offset volume may be visible while its monument marker is off-screen.
            if !footprints.projected_bounds(project).expand(1.).intersects(viewport) {
                continue;
            }
            // A nonconvex source mesh contributes projected surface faces.
            // Their shared edges are internal to the restriction volume.
            let outline = if footprints.polygons.len() == 1 {
                Stroke::new(1.25, Color32::from_rgb(240,124,100))
            } else {
                Stroke::NONE
            };
            for polygon in &footprints.polygons {
                if drawn>=maximum{return (drawn,partial+1);}
                let points: Vec<_> = polygon.iter().copied().map(&project).collect();
                if points.len()<2||!Rect::from_points(&points).expand(1.).intersects(viewport){continue;}
                if points.len()==2 {
                    if !outline.is_empty() {
                        painter.line_segment([points[0], points[1]], outline);
                        drawn += 1;
                    }
                    continue;
                }
                painter.add(egui::Shape::convex_polygon(points,Color32::from_rgba_unmultiplied(240,124,100,22),outline));drawn+=1;
            }
        }
        (drawn,partial)
    }
}

fn prepare_catalog(
    installation: Option<(PathBuf, Installation)>,
    catalog: Option<Arc<Catalog>>,
    cancelled: impl Fn() -> bool,
) -> Result<Arc<Catalog>, String> {
    if let Some(catalog) = catalog.filter(|catalog| Panel::has_reports(catalog)) {
        return Ok(catalog);
    }
    let (root, source) = installation.ok_or("No Rust installation for restriction preparation")?;
    mapper_studio::no_build::prepare(&root, &source, cancelled).map(Arc::new)
}

fn restriction_marker_pose(m: &super::map_monuments::Monument) -> (Option<&str>, [f32; 3]) {
    m.restriction_pose()
        .map_or((None, [0.; 3]), |pose| (m.prefab.as_deref(), pose))
}

fn visible_grid_indices(
    count: u32,
    minimum: f32,
    maximum: f32,
    position: impl Fn(u32) -> f32,
) -> std::ops::Range<u32> {
    let first = (0..count).find(|&i| position(i) >= minimum).unwrap_or(count);
    let end = (first..count)
        .rfind(|&i| position(i) <= maximum)
        .map_or(first, |i| i + 1);
    first..end
}

fn repeating_grid_indices(origin: f32, step: f32, min: f32, max: f32) -> std::ops::Range<i32> {
    ((min - origin) / step).floor() as i32..((max - origin) / step).ceil() as i32
}

fn add_grid_quad(mesh: &mut egui::Mesh, rect: Rect, uv: Rect, color: Color32, map: Rect) {
    // The PaperMap quad covers three terrain widths. The source pixel shader
    // fades the overlay outside the central 40% of the quad on each axis.
    let fade = |point: egui::Pos2| {
        let offset = (point - map.center()) / map.size();
        1. - ((offset.x.abs() - 0.6).max(0.) + (offset.y.abs() - 0.6).max(0.)).min(1.)
    };
    let points = [rect.left_top(), rect.right_top(), rect.left_bottom(), rect.right_bottom()];
    let alpha = points.map(fade);
    if alpha == [0.; 4] {
        return;
    }
    if alpha == [1.; 4] {
        mesh.add_rect_with_uv(rect, uv, color);
        return;
    }
    let first = mesh.vertices.len() as u32;
    mesh.add_triangle(first, first + 1, first + 2);
    mesh.add_triangle(first + 2, first + 1, first + 3);
    mesh.vertices.extend(
        points.into_iter().zip([uv.left_top(), uv.right_top(), uv.left_bottom(), uv.right_bottom()])
            .zip(alpha)
            .map(|((pos, uv), alpha)| egui::epaint::Vertex {
                pos,
                uv,
                color: color.gamma_multiply(alpha),
            }),
    );
}

/// Cross-section of the inspected client's repeating MapGrid texture. Each
/// mip has a one-texel rim around the tile and a clear interior.
fn paper_grid_rim(cell_pixels: f32) -> (f32, f32) {
    const RED: [u8; 10] = [255, 198, 148, 107, 74, 49, 32, 22, 22, 22];
    let mip = (512. / cell_pixels).log2().round().clamp(0., 9.) as usize;
    ((512 >> mip) as f32, RED[mip] as f32 / 255.)
}

fn paper_grid(size: u32, appearance: Option<&mapper_studio::map_grid::Appearance>) -> Option<mapper_studio::map_grid::Grid> {
    appearance.and_then(|style| style.grid(size)).or_else(|| mapper_studio::map_grid::Grid::paper(size))
}

pub(super) fn grid_coordinate(
    map: Rect,
    point: egui::Pos2,
    size: u32,
    appearance: Option<&mapper_studio::map_grid::Appearance>,
) -> Option<String> {
    if !map.is_positive() || !map.contains(point) {
        return None;
    }
    let grid = paper_grid(size, appearance)?;
    // Use the same north-down cell coordinates as the displayed atlas. The
    // east and south edges are exclusive, just like the last shader tile.
    let tile = (point - map.min) / map.size() * grid.nominal_count as f32;
    let cell = tile.floor();
    (cell.x < grid.nominal_count as f32 && cell.y < grid.nominal_count as f32)
        .then(|| mapper_studio::map_grid::label([cell.x as i32, cell.y as i32]))
}

pub fn grid(
    painter: &egui::Painter,
    map: Rect,
    viewport: Rect,
    size: u32,
    appearance: Option<&mapper_studio::map_grid::Appearance>,
) {
    let Some(grid) = paper_grid(size, appearance) else {
        return;
    };
    let line_clip = Rect::from_center_size(map.center(), map.size() * 3.).intersect(viewport);
    if !map.is_positive() || !line_clip.is_positive() {
        return;
    }
    let painter = painter.with_clip_rect(line_clip);
    let cells = grid.nominal_count;
    let step = map.size() / cells as f32;
    let line_x = |i| map.left() + i as f32 * step.x;
    let line_y = |i| map.top() + i as f32 * step.y;
    let pp = painter.ctx().pixels_per_point();
    let cell_pixels = step.x.min(step.y) * pp;
    if let Some((style, (texture, uv, border))) = appearance.and_then(|style| {
        Some((style, style.line_uv(painter.ctx(), cell_pixels)?))
    }) {
        let columns = repeating_grid_indices(map.left(), step.x, line_clip.left(), line_clip.right());
        let rows = repeating_grid_indices(map.top(), step.y, line_clip.top(), line_clip.bottom());
        let color = style.color(false);
        let mut mesh = egui::Mesh::with_texture(texture.id());
        for row in rows {
            for col in columns.clone() {
                let cell = Rect::from_min_max(
                    map.min + egui::vec2(col as f32, row as f32) * step,
                    map.min + egui::vec2((col + 1) as f32, (row + 1) as f32) * step,
                );
                if border == 0.5 {
                    add_grid_quad(&mut mesh, cell, uv, color, map);
                    continue;
                }
                let end = 1. - border;
                for (x, y) in [
                    ([0., 1.], [0., border]),
                    ([0., 1.], [end, 1.]),
                    ([0., border], [border, end]),
                    ([end, 1.], [border, end]),
                ] {
                    let part = |rect: Rect| Rect::from_min_max(
                        rect.lerp_inside(egui::vec2(x[0], y[0])),
                        rect.lerp_inside(egui::vec2(x[1], y[1])),
                    );
                    let rect = part(cell);
                    if rect.intersects(line_clip) {
                        add_grid_quad(&mut mesh, rect, part(uv), color, map);
                    }
                }
            }
        }
        if !mesh.is_empty() {
            painter.add(egui::Shape::mesh(mesh));
        }
    } else {
        paint_legacy_grid_lines(&painter, map, line_clip, step, appearance, pp);
    }
    // The client only samples coordinate glyphs within the terrain, with a
    // separate cutoff at 99% of the terrain width.
    let clip = viewport.intersect(Rect::from_min_max(
        map.min,
        egui::pos2(map.left() + map.width() * 0.99, map.bottom()),
    ));
    if !clip.is_positive() {
        return;
    }
    let painter = painter.with_clip_rect(clip);
    if let Some(style) = appearance {
        let columns = visible_grid_indices(cells, clip.left() - step.x, clip.right(), line_x);
        let rows = visible_grid_indices(cells, clip.top() - step.y, clip.bottom(), line_y);
        let ctx = painter.ctx();
        let cache_id = egui::Id::new("paper-map-grid-atlas");
        let cached = ctx.data(|data| data.get_temp::<(String, egui::TextureHandle)>(cache_id));
        let texture = cached.filter(|(id, _)| id == style.atlas_id()).map(|(_, texture)| texture)
            .unwrap_or_else(|| {
                let texture = ctx.load_texture("PaperMap grid labels",
                    egui::ImageData::Color(style.atlas.clone()), egui::TextureOptions::LINEAR);
                ctx.data_mut(|data| data.insert_temp(cache_id, (style.atlas_id().to_owned(), texture.clone())));
                texture
            });
        let color = style.color(true);
        let mut mesh = egui::Mesh::with_texture(texture.id());
        for row in rows {
            for col in columns.clone() {
                let corner = egui::pos2(line_x(col), line_y(row));
                for glyph in style.glyphs_scaled(col, row, 1.65) {
                    let rect = Rect::from_min_max(
                        corner + glyph.cell_rect.min.to_vec2() * step,
                        corner + glyph.cell_rect.max.to_vec2() * step,
                    );
                    mesh.add_rect_with_uv(rect, glyph.uv, color);
                }
            }
        }
        if !mesh.is_empty() {
            painter.add(egui::Shape::mesh(mesh));
        }
        return;
    }
    if step.x.min(step.y) < 24. {
        return;
    }
    let inset = step * 0.05;
    let label_x = |col| line_x(col) + inset.x;
    let label_y = |row| line_y(row) + inset.y;
    let font = egui::FontId::monospace((step.y * 0.23).clamp(9., 15.));
    let color = Color32::from_rgba_unmultiplied(108, 108, 108, 104);
    let columns = visible_grid_indices(cells, clip.left() - 56., clip.right(), label_x);
    let rows = visible_grid_indices(cells, clip.top() - 19., clip.bottom(), label_y);
    for row in rows {
        let y = label_y(row);
        for col in columns.clone() {
            let text = mapper_studio::map_grid::label([col as i32, row as i32]);
            painter.text(egui::pos2(label_x(col), y), egui::Align2::LEFT_TOP, text, font.clone(), color);
        }
    }
}

fn paint_legacy_grid_lines(
    painter: &egui::Painter,
    map: Rect,
    clip: Rect,
    step: egui::Vec2,
    appearance: Option<&mapper_studio::map_grid::Appearance>,
    pp: f32,
) {
    let (texels, opacity) = paper_grid_rim(step.x.min(step.y) * pp);
    let color = appearance.map_or_else(
        || Color32::from_gray(108),
        |style| style.color(false),
    );
    let mut lines = egui::Mesh::default();
    for (vertical, step, min, max, origin) in [
        (true, step.x, clip.left(), clip.right(), map.left()),
        (false, step.y, clip.top(), clip.bottom(), map.top()),
    ] {
        let texel = step * pp / texels;
        // Below four texture pixels the two sides meet across the tile.
        let solid = texels <= 2.;
        let radius = 1.5 * texel;
        let mut band = |from, to, alpha: f32| {
            let rect = if vertical {
                Rect::from_x_y_ranges(from..=to, clip.y_range())
            } else {
                Rect::from_x_y_ranges(clip.x_range(), from..=to)
            };
            // Split long strips where the coastal fade starts so interpolation
            // leaves the center of the terrain at full opacity.
            let center = if vertical { map.center().y } else { map.center().x };
            let half = if vertical { map.height() } else { map.width() } * 0.6;
            let (start, end) = if vertical { (rect.top(), rect.bottom()) } else { (rect.left(), rect.right()) };
            let breaks = [start, (center - half).clamp(start, end), (center + half).clamp(start, end), end];
            for span in breaks.windows(2).filter(|span| span[0] < span[1]) {
                let rect = if vertical {
                    Rect::from_x_y_ranges(rect.x_range(), span[0]..=span[1])
                } else {
                    Rect::from_x_y_ranges(span[0]..=span[1], rect.y_range())
                };
                add_grid_quad(&mut lines, rect, Rect::from_min_max(egui::epaint::WHITE_UV, egui::epaint::WHITE_UV),
                    color.gamma_multiply(alpha * opacity), map);
            }
        };
        if solid {
            // Both axes read the same solid mip.
            band(min, max, 1.);
            break;
        }
        let position = |i| origin + i as f32 * step;
        let indices = repeating_grid_indices(origin, step, min - radius / pp, max + radius / pp);
        for i in indices {
            let center = position(i) * pp;
            let first = (center - radius).floor().max((min * pp).floor()) as i32;
            let last = (center + radius).ceil().min((max * pp).ceil()) as i32;
            for pixel in first..last {
                let distance = (pixel as f32 + 0.5 - center).abs();
                let alpha = (1.5 - distance / texel).clamp(0., 1.);
                if alpha > 0. {
                    band(pixel as f32 / pp, (pixel + 1) as f32 / pp, alpha);
                }
            }
        }
    }
    if !lines.is_empty() {
        painter.add(egui::Shape::mesh(lines));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paper_grid_coordinates_follow_visible_north_down_tiles() {
        let map = Rect::from_min_size(egui::pos2(100., 75.), egui::vec2(1080., 540.));
        let coordinate = |x, y| grid_coordinate(map, egui::pos2(x, y), 4000, None);
        assert_eq!(coordinate(100., 75.).as_deref(), Some("A0"));
        assert_eq!(coordinate(141., 116.).as_deref(), Some("B2"));
        assert_eq!(coordinate(1179., 614.).as_deref(), Some("AA26"));
        assert_eq!(coordinate(1180., 614.), None);
        assert_eq!(coordinate(1179., 615.), None);
        assert_eq!(coordinate(99.9, 75.), None);
    }

    #[test]
    fn paper_grid_keeps_line_spacing_across_the_terrain_edge() {
        let map = Rect::from_min_max(egui::pos2(100., 100.), egui::pos2(220., 220.));
        let viewport = map.expand(40.);
        let step = map.width() / 6.;
        assert_eq!(repeating_grid_indices(map.left(), step, viewport.left(), viewport.right()), -2..8);

        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput { screen_rect: Some(viewport), ..Default::default() }, |ui| {
            grid(ui.painter(), map, viewport, 1000, None);
        });
        output.textures_delta.clear();
        let lines = output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Mesh(mesh) => Some(mesh),
            _ => None,
        }).unwrap();
        let opacity_at = |x: f32| lines.vertices.iter()
            .filter(|v| (v.pos.x - x).abs() <= 1. && (v.pos.y - 88.).abs() < 0.1)
            .map(|v| v.color.a()).max().unwrap_or(0);
        assert!(opacity_at(80.) > 0);
        assert!(opacity_at(120.) > opacity_at(80.));
    }

    #[test]
    fn surface_restrictions_skip_buried_boxes_and_small_blockers() {
        use mapper_studio::no_build::Geometry;
        let surface = "assets/bundled/prefabs/autospawn/monument/large/airfield_1.prefab";
        let portal = "assets/bundled/prefabs/autospawn/tunnel-upwards/intersection-b5-e.prefab";
        let below = Geometry::Box {
            center: [0., -40., 0.],
            half_axes: [[100., 0., 0.], [0., 10., 0.], [0., 0., 100.]],
            flat_orthogonal: true,
        };
        let underground_rail_bed = Geometry::Box {
            center: [0., 4.5, 0.],
            half_axes: [[90., 0., 0.], [0., 5., 0.], [0., 0., 72.]],
            flat_orthogonal: true,
        };
        let portal_exit = Geometry::Sphere { center: [0., 105., 0.], radius: 70. };
        let small = Geometry::Sphere { center: [0., 2., 0.], radius: 5. };
        let diagonal_wall = Geometry::Box {
            center: [0., 4., 0.],
            half_axes: [[90., 0., 90.], [0., 4., 0.], [-4., 0., 4.]],
            flat_orthogonal: true,
        };
        assert_eq!(surface_area(surface, &below), None);
        assert_eq!(surface_area(surface, &small), None);
        assert_eq!(surface_area(surface, &diagonal_wall), None);
        assert_eq!(surface_area(portal, &underground_rail_bed), None);
        assert!(surface_area(portal, &portal_exit).is_some());
        assert!(!is_surface_prefab("assets/bundled/prefabs/autospawn/monument/cave/cave_large_hard.prefab"));
        assert!(!is_surface_prefab("assets/bundled/prefabs/autospawn/monument/underwater_lab/underwater_lab_a.prefab"));
        assert!(!is_surface_prefab("assets/bundled/prefabs/autospawn/tunnel-base/intersection.prefab"));
        assert_eq!(surface_area(surface, &Geometry::Sphere { center: [0., -50., 0.], radius: 50. }), None);
        let boundary = Geometry::Sphere { center: [0.; 3], radius: 75. };
        let axis = 100. / 2_f32.sqrt();
        let cross = 25. / 2_f32.sqrt();
        let rotated = Geometry::Box {
            center: [0., 2., 0.],
            half_axes: [[axis, 0., axis], [0., 2., 0.], [-cross, 0., cross]],
            flat_orthogonal: true,
        };
        // The diagonal block's bounding rectangle is wider than the sphere,
        // but it encloses less ground than the site's main restriction.
        assert!((surface_area(surface, &rotated).unwrap() - 10_000.).abs() < 0.01);
        assert!(surface_area(surface, &boundary) > surface_area(surface, &rotated));
    }

    #[test]
    fn harbor_retains_three_surface_zones_and_other_monuments_keep_one() {
        use mapper_studio::no_build::{Geometry, Report, Volume};
        let ctx = egui::Context::default();
        let mut app = super::super::tests::fixture(&ctx);
        let path = app.project.targets[0].prefab_path.clone().unwrap();
        app.prefabs.seed_test_stamp(&ctx, &mut app.project, &path);
        let source = app.prefabs.catalog(&app.project).unwrap();
        let mut catalog = Catalog {
            sha256: source.sha256.clone(),
            installation: source.installation.clone(),
            entries: source.entries.clone(),
            sources: source.sources.clone(),
        };
        let report = Report {
            version: mapper_studio::no_build::VERSION,
            space: "prefab-root-local-unity".into(),
            layer_index: 29,
            nodes_visited: 5,
            complete: true,
            issues: Vec::new(),
            should_display_on_map: None,
            volumes: [
                ([0., 0., 0.], 250.),
                ([-300., 0., 0.], 150.),
                ([300., 0., 0.], 100.),
                ([0., 0., 0.], 10.),
                ([0., -300., 0.], 160.),
            ].into_iter().enumerate().map(|(index, (center, radius))| Volume {
                object_id: index as i64 + 1,
                active: true,
                geometry: Geometry::Sphere { center, radius },
            }).collect(),
        };
        let asset = mapper_studio::prefab_assets::Asset {
            priority: None, bounds: None, yaw: None, modifier_size: None,
            modifier_offset: None, texture_guids: Default::default(), splat_mask: None,
            modifier: None, modifier_absent: false, water_geometry: None,
            no_build_geometry: Some(report), display_name: None,
        };
        catalog.entries.insert(path.clone(), asset.clone());
        for variant in ["harbor_1", "harbor_2"] {
            catalog.entries.insert(
                format!("assets/bundled/prefabs/autospawn/monument/harbor/{variant}.prefab"),
                asset.clone(),
            );
        }
        let prepared = Prepared::new(Arc::new(catalog), || false).unwrap();
        assert_eq!(prepared.footprints[&path].polygons.len(), 1);
        for variant in ["harbor_1", "harbor_2"] {
            let path = format!("assets/bundled/prefabs/autospawn/monument/harbor/{variant}.prefab");
            assert_eq!(prepared.footprints[&path].polygons.len(), 3);
        }
    }

    #[test]
    fn result_panels_receive_prepared_reports_without_replacing_recipe_pins() {
        let ctx = egui::Context::default();
        let mut app = super::super::tests::fixture(&ctx);
        let path = app.project.targets[0].prefab_path.clone().unwrap();
        app.prefabs.seed_test_stamp(&ctx, &mut app.project, &path);
        let source = app.prefabs.catalog(&app.project).unwrap();
        app.project.monument_assets = Some(mapper_studio::AssetIdentity {
            catalog_sha256: source.sha256.clone(),
            bindings_sha256: "c".repeat(64),
        });
        let recipe = app.project.clone();
        let mut result = Panel::from_catalog(Some(source.clone()));
        assert!(result.catalog_for(&recipe).is_none());

        let mut entries = source.entries.clone();
        entries.insert(path.clone(), mapper_studio::prefab_assets::Asset {
            priority: None, bounds: None, yaw: None, modifier_size: None,
            modifier_offset: None, texture_guids: Default::default(), splat_mask: None,
            modifier: None, modifier_absent: false, water_geometry: None,
            no_build_geometry: None, display_name: None,
        });
        for asset in entries.values_mut() {
            asset.no_build_geometry = Some(mapper_studio::no_build::Report {
                version: mapper_studio::no_build::VERSION,
                space: "prefab-root-local-unity".into(), layer_index: 29,
                nodes_visited: 1, issues: Vec::new(), complete: true, volumes: Vec::new(),
                should_display_on_map: None,
            });
        }
        let prepared = Catalog {
            sha256: "b".repeat(64), installation: source.installation.clone(),
            entries, sources: source.sources.clone(),
        };
        assert!(Panel::has_reports(&prepared));
        let mut prepared_panel = Panel::default();
        let portal = "assets/bundled/prefabs/autospawn/tunnel-upwards/intersection-bb2-w.prefab";
        prepared_panel.set_tunnel_markers(&recipe, Some(mapper_studio::client_preparation::TunnelMarkerOffsets {
            catalog_sha256: recipe.monument_assets.as_ref().unwrap().catalog_sha256.clone(),
            offsets: [(portal.into(), [-360., 0.])].into_iter().collect(),
        }));
        let (sender, receiver) = mpsc::channel();
        prepared_panel.task = Some(Task { cancel: Arc::new(AtomicBool::new(false)), receiver });
        assert!(prepared_panel.pending_for(&recipe));
        assert!(prepared_panel.catalog_for(&recipe).is_none());
        let prepared = Arc::new(prepared);
        assert!(Prepared::new(prepared.clone(), || true).is_err());
        let ready = Prepared::new(prepared.clone(), || false).unwrap();
        let footprints = ready.footprints.clone();
        sender.send(Ok(ready)).unwrap();
        assert!(prepared_panel.tick(true, &recipe, None, None, &ctx));
        assert!(!prepared_panel.pending_for(&recipe));
        assert!(Arc::ptr_eq(&prepared, &prepared_panel.catalog_for(&recipe).unwrap()));
        assert!(Arc::ptr_eq(&footprints, &prepared_panel.footprints));
        let (sender, receiver) = mpsc::channel();
        sender.send(Prepared::new(prepared.clone(), || false)).unwrap();
        let mut cancelled = Panel {
            source: recipe.installation.clone(),
            attempted: true,
            task: Some(Task { cancel: Arc::new(AtomicBool::new(true)), receiver }),
            ..Panel::default()
        };
        assert!(!cancelled.tick(true, &recipe, None, None, &ctx));
        assert!(cancelled.catalog.is_none());
        assert_eq!(cancelled.message, "No-build preparation cancelled");
        let legacy_marker = super::super::map_monuments::Monument {
            id: 2,
            name: "Rail tunnel entrance".into(),
            prefab: Some(portal.into()),
            position: [10., 15., -260.],
            yaw: 90.,
            restriction_origin: None,
        };
        let mut markers = [legacy_marker.clone(), legacy_marker.clone()];
        markers[1].restriction_origin = Some([200., 200.]);
        result.sync(&recipe, &prepared_panel, &mut markers);
        let [x, z] = markers[0].restriction_origin.unwrap();
        assert!((x - 10.).abs() < 0.0001 && (z - 100.).abs() < 0.0001);
        assert_eq!(markers[1].restriction_origin, Some([200., 200.]));
        assert!(Arc::ptr_eq(&prepared, &result.catalog_for(&recipe).unwrap()));
        assert!(Arc::ptr_eq(&result.footprints, &prepared_panel.footprints));
        result.sync(&recipe, &prepared_panel, &mut []);
        assert!(Arc::ptr_eq(&prepared, &result.catalog_for(&recipe).unwrap()));
        assert_eq!(app.project, recipe);
        assert!(Arc::ptr_eq(&source, &app.prefabs.catalog(&recipe).unwrap()));

        let mut other = recipe.clone();
        other.installation = None;
        assert!(result.catalog_for(&other).is_none());
        let mut empty = Panel::default();
        empty.sync(&other, &prepared_panel, &mut []);
        assert!(empty.catalog.is_none());
        let mut different_catalog = recipe.clone();
        different_catalog.monument_assets.as_mut().unwrap().catalog_sha256 = "f".repeat(64);
        let mut unverified = [legacy_marker];
        empty.sync(&different_catalog, &prepared_panel, &mut unverified);
        assert!(unverified[0].restriction_origin.is_none());
        let mut partial = prepared.entries.clone();
        partial.get_mut(&path).unwrap().no_build_geometry = None;
        assert!(!Panel::has_reports(&Catalog {
            entries: partial, sha256: prepared.sha256.clone(),
            installation: prepared.installation.clone(), sources: prepared.sources.clone(),
        }));
    }

    #[test]
    fn tunnel_overlay_and_export_project_the_prefab_root_instead_of_the_icon() {
        let path = "assets/bundled/prefabs/autospawn/tunnel-upwards/intersection-bb2-w.prefab";
        let panel = Panel {
            footprints: Arc::new(std::collections::HashMap::from([(
                path.to_owned(),
                Footprints::new(vec![vec![[10., 0.], [20., 0.], [20., 10.]]]),
            )])),
            ..Default::default()
        };
        let mut marker = super::super::map_monuments::Monument {
            id: 1,
            name: "Rail tunnel entrance".into(),
            prefab: Some(path.into()),
            position: [-360., 15., 0.],
            yaw: 90.,
            restriction_origin: Some([0., 100.]),
        };
        assert_eq!(restriction_marker_pose(&marker), (Some(path), [0., 100., 90.]));
        assert_eq!(panel.extent(std::slice::from_ref(&marker)), 120.);
        assert!(panel.export_bytes(std::slice::from_ref(&marker)) > 0);
        marker.restriction_origin = None;
        assert_eq!(restriction_marker_pose(&marker), (None, [0.; 3]));
        assert_eq!(panel.extent(std::slice::from_ref(&marker)), 0.);
        assert_eq!(panel.export_bytes(std::slice::from_ref(&marker)), 0);
    }

    #[test]
    fn offshore_restrictions_use_the_visible_ocean_area() {
        let ctx = egui::Context::default();
        let mut app = super::super::tests::fixture(&ctx);
        let path = app.project.targets[0].prefab_path.clone().unwrap();
        app.prefabs.seed_test_stamp(&ctx, &mut app.project, &path);
        let source = app.prefabs.catalog(&app.project).unwrap();
        let mut entries = source.entries.clone();
        let report = mapper_studio::no_build::Report {
            version: 2, space: "prefab-root-local-unity".into(), layer_index: 29,
            nodes_visited: 3, issues: Vec::new(), complete: true,
            should_display_on_map: Some(false),
            volumes: vec![mapper_studio::no_build::Volume {
                object_id: 1, active: true,
                geometry: mapper_studio::no_build::Geometry::Sphere {center: [-500., 0., 0.], radius: 20.},
            }, mapper_studio::no_build::Volume {
                object_id: 2, active: true,
                geometry: mapper_studio::no_build::Geometry::Sphere {center: [-500., 0., 0.], radius: 4.},
            }, mapper_studio::no_build::Volume {
                object_id: 3, active: false,
                geometry: mapper_studio::no_build::Geometry::Sphere {center: [-500., 0., 0.], radius: 100.},
            }, mapper_studio::no_build::Volume {
                object_id: 4, active: true,
                geometry: mapper_studio::no_build::Geometry::Sphere {center: [-500., -150., 0.], radius: 100.},
            }],
        };
        entries.insert(path.clone(), mapper_studio::prefab_assets::Asset {
            priority: None, bounds: None, yaw: None, modifier_size: None,
            modifier_offset: None, texture_guids: Default::default(), splat_mask: None,
            modifier: None, modifier_absent: false, water_geometry: None,
            no_build_geometry: Some(report), display_name: None,
        });
        let panel = Panel::from_catalog(Some(Arc::new(Catalog {
            sha256: source.sha256.clone(), installation: source.installation.clone(),
            entries, sources: source.sources.clone(),
        })));
        assert!(!panel.shows_map_text(Some(&path)));
        assert!(panel.shows_map_text(None));
        assert_eq!(panel.footprints[&path].polygons.len(), 1);
        assert_eq!(panel.footprints[&path].bounds.width(), 40.);
        let map = Rect::from_min_size(egui::pos2(100., 100.), egui::vec2(200., 200.));
        let viewport = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400., 400.));
        let mut output = ctx.run_ui(egui::RawInput {screen_rect: Some(viewport), ..Default::default()}, |ui| {
            let counts = panel.paint_poses(ui.painter(), map, viewport, 1000.,
                [(Some(path.as_str()), [1100., 300., 37.]),
                 (Some(path.as_str()), [1600., 300., 37.])].into_iter(),4096);
            assert_eq!(counts, (1, 0));
        });
        let shape = output.shapes.iter().find(|s| matches!(&s.shape, egui::Shape::Path(_))).unwrap();
        assert!(shape.clip_rect.contains(egui::pos2(340., 200.)));
        assert!(!map.contains(egui::pos2(340., 200.)));
        assert!(!viewport.contains(egui::pos2(420., 140.)));
        output.textures_delta.clear();

        let hidden = super::super::map_monuments::Monument {
            id: 1, name: "Hidden landmark".into(), prefab: Some(path),
            position: [0.; 3], yaw: 0., restriction_origin: None,
        };
        let visible = super::super::map_monuments::Monument {
            id: 2, name: "Visible landmark".into(), prefab: None,
            position: [250., 0., 0.], yaw: 0., restriction_origin: None,
        };
        let (_, metadata) = super::super::export_render::Scene {
            project: app.project, monuments: vec![hidden, visible], restrictions: panel,
            icons: Default::default(), font: None, preferences: Default::default(),
            settings_path: None, background: Color32::TRANSPARENT,
        }.draw(super::super::export_panel::Source::Unavailable, 256,
            mapper_studio::image_render::Settings {
                markers: mapper_studio::image_render::Markers::Text,
                terrain: false,
                ..Default::default()
            }, || false).unwrap().unwrap();
        let markers = metadata.unwrap().monuments;
        assert!(markers[0].label_pixel_bounds.is_none());
        assert!(markers[1].label_pixel_bounds.is_some());
    }
}
