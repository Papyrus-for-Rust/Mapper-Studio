//! Bounded terrain inspection using egui's existing textured triangle renderer.
//! Bounded static model LODs share a texture atlas and triangle batch with terrain.
use eframe::egui::{self, Color32, Rect, Response, TextureId, Vec2};
use std::sync::{Arc, mpsc};
#[derive(Clone, Copy)]
pub struct NativeGround<'a> {
    /// The original composed texture identifies the immutable terrain generation.
    /// Color overrides use a separate texture and can reuse the same geometry.
    pub revision: TextureId,
    pub layers: &'a papyrus_terrain::native_layers::Layers,
    pub surfaces: &'a [[f32; 4]],
}
#[derive(Clone, Copy)]
pub enum Cables<'a> {
    Ready(&'a papyrus_terrain::powerline_wires::Report),
    Unavailable(&'a str),
}

fn cable_quad(
    a: (egui::Pos2, f32),
    b: (egui::Pos2, f32),
    rect: Rect,
) -> Option<([egui::Pos2; 4], f32)> {
    if !a.0.is_finite()
        || !b.0.is_finite()
        || !a.1.is_finite()
        || !b.1.is_finite()
        || a.1 < NEAR
        || b.1 < NEAR
        || !Rect::from_two_pos(a.0, b.0).expand(0.5).intersects(rect)
    {
        return None;
    }
    let direction = b.0 - a.0;
    let length = direction.length();
    if length < 0.75 {
        return None;
    }
    let side = egui::vec2(-direction.y, direction.x) * (0.4 / length);
    Some((
        [a.0 + side, a.0 - side, b.0 + side, b.0 - side],
        a.1 * 0.5 + b.1 * 0.5,
    ))
}
struct TerrainGrid {
    vertices: Vec<[f32; 2]>,
    triangles: Vec<[u32; 3]>,
}

struct TerrainDetail {
    revision: Option<TextureId>,
    size: u32,
    resolution: usize,
    grid: TerrainGrid,
    native_heights: Option<Vec<f32>>,
}

impl TerrainDetail {
    fn new(ground: Option<NativeGround<'_>>, heights: &[i16], resolution: usize, size: u32) -> Self {
        let grid = terrain_grid(&detail_key(ground, size));
        let native_heights = ground.map(|ground| {
            grid.vertices
                .iter()
                .map(|&[u, v]| {
                    papyrus_terrain::spawn::sample_height(ground.layers, u, 1. - v)
                        .unwrap_or_else(|_| preview_height(heights, resolution, u, v))
                })
                .collect()
        });
        Self {
            revision: ground.map(|ground| ground.revision),
            size,
            resolution,
            grid,
            native_heights,
        }
    }
}

fn preview_height(heights: &[i16], resolution: usize, u: f32, v: f32) -> f32 {
    let ix = (u * (resolution - 1) as f32).round() as usize;
    let iz = ((1. - v) * (resolution - 1) as f32).round() as usize;
    papyrus_terrain::erosion::decode(heights[iz * resolution + ix])
}

fn detail_key(ground: Option<NativeGround<'_>>, size: u32) -> Vec<[u16; 4]> {
    let Some(ground) = ground else {
        return Vec::new();
    };
    let pad = size as f32 / 256.;
    let mut key: Vec<_> = ground
        .surfaces
        .iter()
        .take(8192)
        .filter(|b| b.iter().all(|v| v.is_finite()) && b[0] <= b[2] && b[1] <= b[3])
        .map(|b| {
            let x = |v: f32| ((v / size as f32 + 0.5) * 1024.).clamp(0., 1024.);
            let z = |v: f32| ((0.5 - v / size as f32) * 1024.).clamp(0., 1024.);
            [
                x(b[0] - pad).floor() as u16,
                z(b[3] + pad).floor() as u16,
                x(b[2] + pad).ceil() as u16,
                z(b[1] - pad).ceil() as u16,
            ]
        })
        .collect();
    key.sort_unstable();
    key.dedup();
    key
}

/// Coverage at each subdivision scale. A collapsed rectangle can overlap the
/// interior of a coarse cell without overlapping either of its smaller cells.
struct SurfaceCoverage {
    levels: [Vec<i32>; 4],
}

impl SurfaceCoverage {
    fn new(rects: &[[u16; 4]]) -> Self {
        let levels = std::array::from_fn(|level| {
            let side = 64 << level;
            let step = 1024 / side;
            let mut table = vec![0; side * side];
            // Mark range corners, then sweep rows and columns to find the
            // number of rectangles intersecting each aligned cell.
            for &[x, z, right, bottom] in rects {
                let (x, z) = (usize::from(x) / step, usize::from(z) / step);
                let (right, bottom) = (
                    usize::from(right).div_ceil(step),
                    usize::from(bottom).div_ceil(step),
                );
                for (col, row, delta) in [
                    (x, z, 1),
                    (right, z, -1),
                    (x, bottom, -1),
                    (right, bottom, 1),
                ] {
                    if col < side && row < side {
                        table[row * side + col] += delta;
                    }
                }
            }
            let mut column_deltas = [0; 512];
            for row in table.chunks_exact_mut(side) {
                let mut active_rects = 0;
                for (cell, delta) in row.iter_mut().zip(&mut column_deltas) {
                    *delta += *cell;
                    active_rects += *delta;
                    *cell = active_rects;
                }
            }
            table
        });
        Self { levels }
    }

    fn intersects(&self, [x, z, right, _]: [u16; 4]) -> bool {
        let step = usize::from(right - x);
        let level = 4 - step.ilog2() as usize;
        let side = 1024 / step;
        self.levels[level][usize::from(z) / step * side + usize::from(x) / step] > 0
    }
}

fn terrain_grid(rects: &[[u16; 4]]) -> TerrainGrid {
    if rects.is_empty() {
        let vertices = (0..=64)
            .flat_map(|z| (0..=64).map(move |x| [x as f32 / 64., z as f32 / 64.]))
            .collect();
        let triangles = (0..64)
            .flat_map(|z| {
                (0..64).flat_map(move |x| {
                    let a = (z * 65 + x) as u32;
                    [[a, a + 1, a + 65], [a + 1, a + 66, a + 65]]
                })
            })
            .collect();
        return TerrainGrid {
            vertices,
            triangles,
        };
    }
    let coverage = (rects.len() >= 128).then(|| SurfaceCoverage::new(rects));
    terrain_grid_refined(|[x, z, r, b]| {
        if let Some(coverage) = &coverage {
            coverage.intersects([x, z, r, b])
        } else {
            rects
                .iter()
                .any(|q| x < q[2] && r > q[0] && z < q[3] && b > q[1])
        }
    })
}

fn terrain_grid_refined(intersects: impl Fn([u16; 4]) -> bool) -> TerrainGrid {
    let mut cells: Vec<[u16; 4]> = (0..64)
        .flat_map(|z| (0..64).map(move |x| [x * 16, z * 16, (x + 1) * 16, (z + 1) * 16]))
        .collect();
    let mut extra = 12288 - 4096;
    // Refine breadth-first so every surface gets detail before the finest pass.
    for _ in 0..4 {
        let mut next = Vec::new();
        for [x, z, r, b] in cells {
            if extra >= 3 && r - x > 1 && intersects([x, z, r, b]) {
                let (mx, mz) = ((x + r) / 2, (z + b) / 2);
                extra -= 3;
                next.extend([
                    [x, z, mx, mz],
                    [mx, z, r, mz],
                    [x, mz, mx, b],
                    [mx, mz, r, b],
                ]);
            } else {
                next.push([x, z, r, b]);
            }
        }
        cells = next;
    }
    // Collect the exact shared edge vertices for neighboring cells. Sorted
    // coordinate lists retain the same order with fewer small allocations.
    let mut horizontal = vec![Vec::new(); 1025];
    let mut vertical = vec![Vec::new(); 1025];
    for &[x, z, r, b] in &cells {
        for row in [z, b] {
            horizontal[usize::from(row)].extend([x, r]);
        }
        for col in [x, r] {
            vertical[usize::from(col)].extend([z, b]);
        }
    }
    for points in horizontal.iter_mut().chain(&mut vertical) {
        points.sort_unstable();
        points.dedup();
    }
    let mut grid = TerrainGrid {
        vertices: Vec::with_capacity(cells.len() * 5),
        triangles: Vec::with_capacity(cells.len() * 4),
    };
    let mut edge = Vec::new();
    for [x, z, r, b] in cells {
        edge.clear();
        edge.extend(
            edge_points(&horizontal[usize::from(z)], x, r)
                .iter()
                .map(|&u| [u, z]),
        );
        edge.extend(
            edge_points(&vertical[usize::from(r)], z, b)
                .iter()
                .map(|&v| [r, v]),
        );
        edge.extend(
            edge_points(&horizontal[usize::from(b)], x + 1, r + 1)
                .iter()
                .rev()
                .map(|&u| [u, b]),
        );
        edge.extend(
            edge_points(&vertical[usize::from(x)], z + 1, b + 1)
                .iter()
                .rev()
                .map(|&v| [x, v]),
        );
        let center = grid.vertices.len() as u32;
        grid.vertices
            .push([(x as f32 + r as f32) / 2048., (z as f32 + b as f32) / 2048.]);
        grid.vertices.extend(
            edge.iter()
                .map(|p| [p[0] as f32 / 1024., p[1] as f32 / 1024.]),
        );
        for i in 0..edge.len() {
            grid.triangles.push([
                center,
                center + 1 + i as u32,
                center + 1 + ((i + 1) % edge.len()) as u32,
            ]);
        }
    }
    grid
}

fn edge_points(points: &[u16], start: u16, end: u16) -> &[u16] {
    let first = points.partition_point(|&point| point < start);
    let last = points.partition_point(|&point| point < end);
    &points[first..last]
}

#[derive(Clone, Copy)]
pub enum Geometry<'a> {
    Static(&'a mapper_studio::monument_source::Model),
    Surface(&'a papyrus_terrain::path_mesh::Template),
}
impl Geometry<'_> {
    fn vertices(&self) -> &[[f32; 3]] {
        match self {
            Self::Static(m) => &m.vertices,
            Self::Surface(m) => &m.vertices,
        }
    }
    fn triangles(&self) -> &[[u32; 3]] {
        match self {
            Self::Static(m) => &m.triangles,
            Self::Surface(m) => &m.triangles,
        }
    }
}
/// Borrow the coarsest retained source LOD; no vertex/index buffers are copied.
pub fn surface_instances(
    maps: &papyrus_terrain::monument_composition::Composed,
) -> impl Iterator<Item = ModelInstance<'_>> {
    let roads = maps
        .roads
        .as_ref()
        .and_then(|roads| roads.surfaces.as_ref())
        .map_or(&[][..], |surfaces| surfaces.chunks.as_slice());
    let rails = maps
        .rails
        .as_ref()
        .map_or(&[][..], |scene| scene.meshes.as_slice());
    let road_lod = roads.iter().map(|chunk| chunk.lod).max();
    let rail_lod = rails.iter().map(|chunk| chunk.lod).max();
    roads.iter()
        .filter(move |chunk| Some(chunk.lod) == road_lod)
        .take(8192)
        .map(|chunk| {
            ModelInstance::surface(&chunk.geometry.mesh, chunk.geometry.origin, [64., 67., 70.])
        })
        .chain(
            rails
                .iter()
                .filter(move |chunk| Some(chunk.lod) == rail_lod)
                .take(8192)
                .map(|chunk| {
                    ModelInstance::surface(
                        &chunk.geometry.mesh,
                        chunk.geometry.origin,
                        [136., 139., 143.],
                    )
                }),
        )
}
pub fn surface_bounds<'a>(instances: impl IntoIterator<Item = ModelInstance<'a>>) -> Vec<[f32; 4]> {
    instances
        .into_iter()
        .filter_map(|i| match i.model {
            Geometry::Surface(m) if i.valid() => {
                let mut bounds = [
                    f32::INFINITY,
                    f32::INFINITY,
                    f32::NEG_INFINITY,
                    f32::NEG_INFINITY,
                ];
                for corner in 0..8 {
                    let world = i.world(std::array::from_fn(|axis| {
                        m.center[axis]
                            + if corner & (1 << axis) == 0 {
                                -m.extents[axis]
                            } else {
                                m.extents[axis]
                            }
                    }));
                    bounds[0] = bounds[0].min(world[0]);
                    bounds[1] = bounds[1].min(world[2]);
                    bounds[2] = bounds[2].max(world[0]);
                    bounds[3] = bounds[3].max(world[2]);
                }
                Some(bounds)
            }
            _ => None,
        })
        .collect()
}
pub struct ModelInstance<'a> {
    pub model: Geometry<'a>,
    pub position: [f32; 3],
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
    pub tint: Option<[f32; 3]>,
}

impl<'a> ModelInstance<'a> {
    pub fn yaw(
        model: &'a mapper_studio::monument_source::Model,
        position: [f32; 3],
        yaw: f32,
    ) -> Self {
        let (s, c) = (yaw.to_radians() * 0.5).sin_cos();
        Self {
            model: Geometry::Static(model),
            position,
            rotation: [0., -s, 0., c],
            scale: [1.; 3],
            tint: None,
        }
    }
    pub fn surface(
        model: &'a papyrus_terrain::path_mesh::Template,
        position: [f32; 3],
        tint: [f32; 3],
    ) -> Self {
        Self {
            model: Geometry::Surface(model),
            position,
            rotation: [0., 0., 0., 1.],
            scale: [1.; 3],
            tint: Some(tint),
        }
    }
    fn world(&self, p: [f32; 3]) -> [f32; 3] {
        let p = papyrus_terrain::decor_fit::rotate(
            self.rotation,
            std::array::from_fn(|i| p[i] * self.scale[i]),
        );
        std::array::from_fn(|i| p[i] + self.position[i])
    }
    fn valid(&self) -> bool {
        let bounds_valid = match self.model {
            Geometry::Static(_) => true,
            Geometry::Surface(m) => {
                m.center.iter().chain(&m.extents).all(|v| v.is_finite())
                    && m.extents.iter().all(|v| *v >= 0.)
            }
        };
        bounds_valid
            && self
                .position
                .iter()
                .chain(&self.rotation)
                .chain(&self.scale)
                .all(|v| v.is_finite())
            && self.scale.iter().all(|v| *v > 0. && *v <= 100.)
            && (self.rotation.iter().map(|v| v * v).sum::<f32>() - 1.).abs() < 0.001
    }
}

/// Physical events prevent rotation's smooth-scroll tail from zooming after release.
pub fn wheel_delta(ui: &egui::Ui) -> f32 {
    ui.input(|i| {
        i.raw
            .events
            .iter()
            .filter_map(|event| {
                if let egui::Event::MouseWheel { unit, delta, .. } = event {
                    Some(
                        delta.y
                            * match unit {
                                egui::MouseWheelUnit::Point => 1.,
                                egui::MouseWheelUnit::Line => 40.,
                                egui::MouseWheelUnit::Page => 400.,
                            },
                    )
                } else {
                    None
                }
            })
            .sum()
    })
}

const NEAR: f32 = 0.001;

type DetailKey = (TextureId, Option<TextureId>, usize, u32);

struct DetailTask {
    key: DetailKey,
    result: mpsc::Receiver<(egui::ColorImage, TerrainDetail)>,
}

#[derive(Clone, Copy)]
struct ViewVertex {
    point: Vec2,
    depth: f32,
    uv: egui::Pos2,
}

fn clip_triangle(corners: [ViewVertex; 3]) -> ([ViewVertex; 4], usize) {
    let mut polygon = [corners[0]; 4];
    let mut count = 0;
    let mut previous = corners[2];
    for vertex in corners {
        let inside = vertex.depth >= NEAR;
        if inside != (previous.depth >= NEAR) {
            let t = (NEAR - previous.depth) / (vertex.depth - previous.depth);
            polygon[count] = ViewVertex {
                point: egui::lerp(previous.point..=vertex.point, t),
                depth: NEAR,
                uv: previous.uv.lerp(vertex.uv, t),
            };
            count += 1;
        }
        if inside {
            polygon[count] = vertex;
            count += 1;
        }
        previous = vertex;
    }
    (polygon, count)
}

fn atlas_image(image: &egui::ColorImage) -> egui::ColorImage {
    let [width, height] = image.size;
    let mut pixels = image.pixels.clone();
    pixels.extend(std::iter::repeat_n(Color32::WHITE, width));
    egui::ColorImage::new([width, height + 1], pixels)
}

fn valid_image(image: &egui::ColorImage) -> bool {
    let [width, height] = image.size;
    (1..=4096).contains(&width) && (1..=4096).contains(&height)
        && image.pixels.len() == width * height
}

pub struct Camera {
    pub enabled: bool,
    yaw: f32,
    pitch: f32,
    eye: [f32; 3],
    speed: f32,
    captured: bool,
    raw_look: bool,
    drawn: bool,
    pointer_drag: Option<(egui::PointerButton, egui::Pos2)>,
    atlas: Option<(TextureId, egui::TextureHandle)>,
    detail: Option<TerrainDetail>,
    details_task: Option<DetailTask>,
    loaded_detail: Option<DetailKey>,
}
impl Default for Camera {
    fn default() -> Self {
        let yaw: f32 = -0.4;
        let pitch: f32 = 0.78;
        let distance = 1.56;
        Self {
            enabled: false,
            yaw,
            pitch,
            eye: [yaw.sin() * pitch.cos() * distance, pitch.sin() * distance,
                yaw.cos() * pitch.cos() * distance],
            speed: 0.22,
            captured: false,
            raw_look: false,
            drawn: false,
            pointer_drag: None,
            atlas: None,
            detail: None,
            details_task: None,
            loaded_detail: None,
        }
    }
}
impl Camera {
    fn terrain_detail(
        &mut self,
        ground: Option<NativeGround<'_>>,
        heights: &[i16],
        resolution: usize,
        size: u32,
    ) -> &TerrainDetail {
        let revision = ground.map(|ground| ground.revision);
        if self.detail.as_ref().is_none_or(|detail| {
            detail.revision != revision || detail.size != size || detail.resolution != resolution
        }) {
            self.detail = Some(TerrainDetail::new(ground, heights, resolution, size));
        }
        self.detail.as_ref().unwrap()
    }

    fn forward(&self) -> [f32; 3] {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        [-sy * cp, -sp, -cy * cp]
    }

    fn translate(&mut self, forward: f32, right: f32, up: f32) {
        let direction = self.forward();
        let (sy, cy) = self.yaw.sin_cos();
        let movement = [right * cy, up, -right * sy];
        for (axis, coordinate) in self.eye.iter_mut().enumerate() {
            *coordinate = (*coordinate + direction[axis] * forward + movement[axis]).clamp(-8., 8.);
        }
    }

    fn look(&mut self, delta: Vec2) {
        self.yaw = (self.yaw - delta.x * 0.004).rem_euclid(std::f32::consts::TAU);
        self.pitch = (self.pitch + delta.y * 0.004).clamp(-1.56, 1.56);
    }

    fn move_pointer(&mut self, position: egui::Pos2, relative: bool) {
        if let Some((button, previous)) = self.pointer_drag {
            let delta = position - previous;
            if button == egui::PointerButton::Middle {
                self.translate(0., -delta.x * 0.001, delta.y * 0.001);
            } else if !relative {
                self.look(delta);
            }
            self.pointer_drag = Some((button, position));
        }
    }

    fn capture(&mut self, ctx: &egui::Context, grab: bool) {
        if self.captured == grab {
            return;
        }
        self.captured = grab;
        let mode = if !grab {
            egui::CursorGrab::None
        } else if cfg!(windows) {
            egui::CursorGrab::Confined
        } else {
            egui::CursorGrab::Locked
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(mode));
        ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(!grab));
    }

    pub fn end_frame(&mut self, ctx: &egui::Context) {
        if !std::mem::take(&mut self.drawn) {
            self.capture(ctx, false);
            self.pointer_drag = None;
            self.details_task = None;
        }
    }

    fn pointer_input(&mut self, ui: &egui::Ui, response: &Response) {
        use egui::{Event, Key, PointerButton as Button};
        let events = ui.input(|i| i.raw.events.clone());
        let focused = ui.input(|i| i.focused && !i.key_pressed(Key::Escape));
        if !focused {
            self.pointer_drag = None;
            self.capture(ui.ctx(), false);
            return;
        }
        let relative = events.iter().any(|event| matches!(event, Event::MouseMoved(_)));
        for event in events {
            match event {
                Event::PointerButton { pos, button, pressed: true, .. }
                    if matches!(button, Button::Primary | Button::Secondary | Button::Middle)
                        && self.pointer_drag.is_none() && response.interact_rect.contains(pos)
                        && ui.ctx().layer_id_at(pos) == Some(ui.layer_id()) =>
                {
                    self.pointer_drag = Some((button, pos));
                    self.raw_look = false;
                }
                Event::MouseMoved(delta)
                    if self.pointer_drag.is_some_and(|(button, _)| button == Button::Secondary) =>
                {
                    self.raw_look = true;
                    self.look(delta);
                }
                Event::PointerMoved(pos) => self.move_pointer(pos, (relative || self.raw_look)
                    && self.pointer_drag.is_some_and(|(button, _)| button == Button::Secondary)),
                Event::PointerButton { pos, button, pressed: false, .. }
                    if self.pointer_drag.is_some_and(|(owned, _)| owned == button) =>
                {
                    self.move_pointer(pos, (relative || self.raw_look) && button == Button::Secondary);
                    self.pointer_drag = None;
                }
                Event::PointerGone if !self.captured => self.pointer_drag = None,
                Event::WindowFocused(false) => self.pointer_drag = None,
                _ => {}
            }
        }
        if self.pointer_drag.is_some_and(|(button, _)| !ui.input(|i| i.pointer.button_down(button))) {
            self.pointer_drag = None;
        }
        let flying = self.pointer_drag.is_some_and(|(button, _)| button == Button::Secondary);
        self.capture(ui.ctx(), flying);
        let scroll = if flying || response.hovered() { wheel_delta(ui) } else { 0. };
        if flying {
            ui.ctx().set_cursor_icon(egui::CursorIcon::None);
            self.speed = (self.speed * (scroll * 0.004).exp()).clamp(0.008, 2.);
            let (direction, step) = ui.input(|input| {
                let axis = |positive, negative| i32::from(input.key_down(positive))
                    as f32 - i32::from(input.key_down(negative)) as f32;
                ([axis(Key::W, Key::S), axis(Key::D, Key::A), axis(Key::E, Key::Q)],
                    self.speed * input.stable_dt.clamp(0., 0.05)
                        * if input.modifiers.shift { 3. } else { 1. })
            });
            let length = direction.iter().map(|value| value * value).sum::<f32>().sqrt();
            if length > 0. {
                let [forward, right, up] = direction.map(|value| value * step / length);
                self.translate(forward, right, up);
                ui.ctx().request_repaint();
            }
        } else if scroll != 0. {
            self.translate((scroll * 0.002).clamp(-0.4, 0.4), 0., 0.);
        }
    }
    pub fn reset(&mut self) {
        let atlas = self.atlas.take();
        let detail = self.detail.take();
        *self = Self {
            enabled: self.enabled,
            captured: self.captured,
            drawn: self.drawn,
            details_task: self.details_task.take(),
            loaded_detail: self.loaded_detail,
            atlas,
            detail,
            ..Self::default()
        };
    }
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)] // Explicit bounded heightfield inputs; no owned grid copies.
    pub fn draw(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        response: &Response,
        texture: TextureId,
        image: &egui::ColorImage,
        heights: &[i16],
        resolution: usize,
        size: u32,
        models: &[ModelInstance<'_>],
    ) {
        self.draw_with_ground(
            ui, rect, response, texture, image, heights, resolution, size, models, None,
        )
    }
    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)] // Retained by the headless road and rail inspection tools.
    pub fn draw_with_ground(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        response: &Response,
        texture: TextureId,
        image: &egui::ColorImage,
        heights: &[i16],
        resolution: usize,
        size: u32,
        models: &[ModelInstance<'_>],
        ground: Option<NativeGround<'_>>,
    ) {
        self.draw_with_scene(
            ui, rect, response, texture, image, heights, resolution, size, models, ground, None,
        );
    }
    #[allow(clippy::too_many_arguments)]
    pub fn draw_with_scene(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        response: &Response,
        texture: TextureId,
        image: &egui::ColorImage,
        heights: &[i16],
        resolution: usize,
        size: u32,
        models: &[ModelInstance<'_>],
        ground: Option<NativeGround<'_>>,
        cables: Option<Cables<'_>>,
    ) {
        if !valid_image(image) || !(2..=513).contains(&resolution)
            || heights.len() != resolution * resolution || size == 0
        {
            return;
        }
        if self.atlas.as_ref().is_none_or(|(id, _)| *id != texture) {
            self.upload_atlas(ui.ctx(), texture, atlas_image(image));
        }
        self.terrain_detail(ground, heights, resolution, size);
        self.render(ui, rect, response, texture, image.size, heights, resolution, size, models, cables);
    }

    fn upload_atlas(&mut self, ctx: &egui::Context, texture: TextureId, image: egui::ColorImage) {
        self.atlas = Some((texture, ctx.load_texture("terrain-model-atlas", image, egui::TextureOptions::LINEAR)));
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw_live(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        response: &Response,
        texture: TextureId,
        image: &Arc<egui::ColorImage>,
        heights: &[i16],
        resolution: usize,
        size: u32,
        models: &[ModelInstance<'_>],
        ground: Option<(TextureId, &Arc<papyrus_terrain::monument_composition::Composed>, &[[f32; 4]])>,
        cables: Option<Cables<'_>>,
    ) -> bool {
        if !valid_image(image) || !(2..=513).contains(&resolution)
            || heights.len() != resolution * resolution || size == 0
        {
            return false;
        }
        let key = (texture, ground.map(|(revision, _, _)| revision), resolution, size);
        if self.loaded_detail != Some(key) {
            self.terrain_detail(None, heights, resolution, size);
            // Keep at most one refinement job alive while a palette or map changes.
            if let Some(task) = &self.details_task
                && let result @ (Ok(_) | Err(mpsc::TryRecvError::Disconnected)) = task.result.try_recv()
            {
                if task.key == key && let Ok((atlas, detail)) = result {
                    self.upload_atlas(ui.ctx(), texture, atlas);
                    self.detail = Some(detail);
                    self.loaded_detail = Some(key);
                }
                self.details_task = None;
            }
            if self.loaded_detail != Some(key) && self.details_task.is_none() {
                let (sender, result) = mpsc::channel();
                let image = image.clone();
                let heights = heights.to_vec();
                let ground = ground.map(|(id, maps, surfaces)| (id, maps.clone(), surfaces.to_vec()));
                let repaint = ui.ctx().clone();
                std::thread::spawn(move || {
                    let atlas = atlas_image(&image);
                    let ground = ground.as_ref().map(|(revision, maps, surfaces)| NativeGround {
                        revision: *revision, layers: &maps.layers, surfaces,
                    });
                    let detail = TerrainDetail::new(ground, &heights, resolution, size);
                    let _ = sender.send((atlas, detail));
                    repaint.request_repaint();
                });
                self.details_task = Some(DetailTask { key, result });
            }
        }
        self.render(ui, rect, response, texture, image.size, heights, resolution, size, models, cables);
        self.loaded_detail != Some(key)
    }

    #[allow(clippy::too_many_arguments)]
    fn render(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        response: &Response,
        texture: TextureId,
        [width, height]: [usize; 2],
        heights: &[i16],
        resolution: usize,
        size: u32,
        models: &[ModelInstance<'_>],
        cables: Option<Cables<'_>>,
    ) {
        self.drawn = true;
        self.pointer_input(ui, response);
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        let eye = self.eye;
        let focal = rect.width().min(rect.height()) * 0.94;
        let view = |x: f32, y: f32, z: f32| {
            let [x, y, z] = [x - eye[0], y - eye[1], z - eye[2]];
            let across = x * cy - z * sy;
            let back = x * sy + z * cy;
            (egui::vec2(across, back * sp - y * cp), -back * cp - y * sp)
        };
        let project = |point: Vec2, depth: f32| {
            rect.center() + point * (focal / depth.max(NEAR))
        };
        let project_world = |x, y, z| {
            let (point, depth) = view(x, y, z);
            (project(point, depth), depth)
        };
        let detail = self.detail.as_ref().unwrap();
        let grid = &detail.grid;
        let atlas = self.atlas.as_ref().filter(|(id, _)| *id == texture);
        let mut mesh = egui::Mesh::with_texture(atlas.map_or(texture, |(_, atlas)| atlas.id()));
        let mut terrain = Vec::with_capacity(grid.vertices.len());
        for (index, &[u, v]) in grid.vertices.iter().enumerate() {
            let elevation = detail.native_heights.as_ref().map_or_else(
                || preview_height(heights, resolution, u, v),
                |samples| samples[index],
            );
            let (point, depth) = view(u - 0.5, elevation.max(0.) / size as f32, v - 0.5);
            let uv = if atlas.is_some() {
                egui::pos2(
                    (u * (width - 1) as f32 + 0.5) / width as f32,
                    (v * (height - 1) as f32 + 0.5) / (height + 1) as f32,
                )
            } else {
                egui::pos2(u, v)
            };
            mesh.vertices.push(egui::epaint::Vertex { pos: project(point, depth), uv, color: Color32::WHITE });
            terrain.push(ViewVertex { point, depth, uv });
        }
        let mut triangles = Vec::with_capacity(grid.triangles.len());
        for &indices in &grid.triangles {
            let corners = indices.map(|index| terrain[index as usize]);
            if corners.iter().all(|vertex| vertex.depth >= NEAR) {
                triangles.push((corners.iter().map(|vertex| vertex.depth).sum::<f32>() / 3., indices));
            } else if corners.iter().any(|vertex| vertex.depth >= NEAR) {
                let (polygon, count) = clip_triangle(corners);
                let base = mesh.vertices.len() as u32;
                for vertex in &polygon[..count] {
                    mesh.vertices.push(egui::epaint::Vertex {
                        pos: project(vertex.point, vertex.depth), uv: vertex.uv, color: Color32::WHITE,
                    });
                }
                for i in 1..count - 1 {
                    let depth = (polygon[0].depth + polygon[i].depth + polygon[i + 1].depth) / 3.;
                    triangles.push((depth, [base, base + i as u32, base + i as u32 + 1]));
                }
            }
        }
        let white_uv = egui::pos2(0.5 / width as f32, (height as f32 + 0.5) / (height + 1) as f32);
        let models = if atlas.is_some() { models } else { &[] };
        let cables = cables.filter(|_| atlas.is_some());
        let mut triangle_work = 0;
        let mut cable_count = 0;
        if let Some(Cables::Ready(report)) = cables {
            for span in report.spans.iter().take(100_000) {
                if triangle_work + 2 > 48_000 {
                    break;
                }
                let endpoints = [span.start, span.end].map(|p| {
                    project_world(p[0] / size as f32, p[1] / size as f32, -p[2] / size as f32)
                });
                let Some((quad, depth)) = cable_quad(endpoints[0], endpoints[1], rect) else {
                    continue;
                };
                let base = mesh.vertices.len() as u32;
                for pos in quad {
                    mesh.vertices.push(egui::epaint::Vertex {
                        pos,
                        uv: white_uv,
                        color: Color32::from_rgb(40, 42, 38),
                    });
                }
                triangles.push((depth, [base, base + 1, base + 2]));
                triangles.push((depth, [base + 2, base + 1, base + 3]));
                triangle_work += 2;
                cable_count += 1;
            }
        }
        let mut shown = 0;
        // Cache local bounds per unique model; thousands of repeated rocks do
        // not rescan their vertex buffers for every instance on every frame.
        let mut bounds_by_model = std::collections::HashMap::new();
        let mut visible = Vec::new();
        for (order, instance) in models.iter().take(100000).enumerate() {
            if !instance.valid()
                || instance.model.triangles().is_empty()
                || instance.model.vertices().is_empty()
            {
                continue;
            }
            let (lower, upper) = match instance.model {
                Geometry::Surface(m) => (
                    std::array::from_fn(|i| m.center[i] - m.extents[i]),
                    std::array::from_fn(|i| m.center[i] + m.extents[i]),
                ),
                Geometry::Static(_) => *bounds_by_model
                    .entry(instance.model.vertices().as_ptr())
                    .or_insert_with(|| {
                        let mut lower = [f32::INFINITY; 3];
                        let mut upper = [f32::NEG_INFINITY; 3];
                        for p in instance.model.vertices() {
                            for i in 0..3 {
                                lower[i] = lower[i].min(p[i]);
                                upper[i] = upper[i].max(p[i]);
                            }
                        }
                        (lower, upper)
                    }),
            };
            let mut screen = Rect::NOTHING;
            let mut crosses_near = false;
            for mask in 0..8 {
                let p = instance.world(std::array::from_fn(|i| {
                    if mask & (1 << i) == 0 {
                        lower[i]
                    } else {
                        upper[i]
                    }
                }));
                let (pos, depth) =
                    project_world(p[0] / size as f32, p[1] / size as f32, -p[2] / size as f32);
                if !pos.is_finite() || !depth.is_finite() || depth < NEAR {
                    crosses_near = true;
                    continue;
                }
                screen.extend_with(pos);
            }
            if crosses_near && screen.is_positive() {
                screen = rect;
            }
            if screen.intersects(rect) && screen.width().max(screen.height()) >= 0.75 {
                visible.push((
                    screen.width().max(0.5) * screen.height().max(0.5),
                    order,
                    instance,
                ));
            }
        }
        visible.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let mut projected_vertices = Vec::new();
        for (_, _, instance) in visible {
            let faces = instance.model.triangles();
            let vertices = instance.model.vertices();
            if triangle_work + faces.len() > 48000 {
                continue;
            }
            projected_vertices.clear();
            // Avoid allocating in proportion to unreferenced vertices on
            // sparsely indexed models.
            if faces.len() > 1 && vertices.len() < faces.len() * 3 {
                projected_vertices.resize(vertices.len(), None);
            }
            let project_vertex = |point: [f32; 3]| {
                let world = instance.world(point);
                let (point, depth) = view(
                    world[0] / size as f32,
                    world[1] / size as f32,
                    -world[2] / size as f32,
                );
                (world, ViewVertex { point, depth, uv: white_uv })
            };
            let mut drew = false;
            for triangle in faces {
                triangle_work += 1;
                let corners = (*triangle).map(|i| {
                    let index = i as usize;
                    let point = *vertices.get(index)?;
                    Some(if let Some(entry) = projected_vertices.get_mut(index) {
                        *entry.get_or_insert_with(|| project_vertex(point))
                    } else {
                        project_vertex(point)
                    })
                });
                let [Some(a), Some(b), Some(c)] = corners else {
                    continue;
                };
                let world = [a.0, b.0, c.0];
                let projected = [a.1, b.1, c.1];
                if projected.iter().any(|v| !v.point.is_finite() || !v.depth.is_finite()) {
                    continue;
                }
                let (polygon, count) = clip_triangle(projected);
                if count < 3 {
                    continue;
                }
                let face_count = count - 2;
                if triangle_work + face_count - 1 > 48_000 {
                    break;
                }
                triangle_work += face_count - 1;
                let a = [
                    world[1][0] - world[0][0],
                    world[1][1] - world[0][1],
                    world[1][2] - world[0][2],
                ];
                let b = [
                    world[2][0] - world[0][0],
                    world[2][1] - world[0][1],
                    world[2][2] - world[0][2],
                ];
                let normal = [
                    a[1] * b[2] - a[2] * b[1],
                    a[2] * b[0] - a[0] * b[2],
                    a[0] * b[1] - a[1] * b[0],
                ];
                let length = normal.iter().map(|n| n * n).sum::<f32>().sqrt();
                if length <= 1e-8 {
                    continue;
                }
                let shade = (0.4
                    + (normal[0] * 0.3 + normal[1] * 0.8 - normal[2] * 0.4).abs() / length * 0.55)
                    .clamp(0.35, 1.);
                let tint = instance.tint.unwrap_or([190., 186., 174.]);
                let color = Color32::from_rgb(
                    (tint[0] * shade) as u8,
                    (tint[1] * shade) as u8,
                    (tint[2] * shade) as u8,
                );
                let base = mesh.vertices.len() as u32;
                for vertex in &polygon[..count] {
                    mesh.vertices.push(egui::epaint::Vertex {
                        pos: project(vertex.point, vertex.depth),
                        uv: white_uv,
                        color,
                    });
                }
                for i in 1..count - 1 {
                    let depth = (polygon[0].depth + polygon[i].depth + polygon[i + 1].depth) / 3.;
                    triangles.push((depth, [base, base + i as u32, base + i as u32 + 1]));
                }
                drew = true;
            }
            shown += usize::from(drew);
        }
        // One atlas/batch; painter ordering is an inspection approximation, not a depth buffer.
        triangles.sort_unstable_by(|a, b| b.0.total_cmp(&a.0));
        for (_, indices) in triangles {
            mesh.indices.extend_from_slice(&indices);
        }
        ui.painter()
            .with_clip_rect(rect)
            .add(egui::Shape::mesh(mesh));
        let mut labels = Vec::new();
        if !models.is_empty() {
            labels.push(format!("{shown}/{} models", models.len()));
        }
        match cables {
            Some(Cables::Ready(report)) if !report.spans.is_empty() => {
                labels.push(format!("{cable_count}/{} cable spans", report.spans.len()))
            }
            Some(Cables::Unavailable(_)) => labels.push("Cables unavailable".into()),
            _ => {}
        }
        if !labels.is_empty() {
            let mut job = egui::text::LayoutJob::simple_singleline(
                labels.join(" · "),
                egui::FontId::proportional(12.),
                ui.visuals().text_color(),
            );
            job.wrap.max_width = (rect.width() - 28.).max(1.);
            job.wrap.max_rows = 1;
            job.wrap.overflow_character = Some('…');
            let galley = ui.painter().layout_job(job);
            let position = rect.left_top() + Vec2::splat(10.);
            let bounds = Rect::from_min_size(
                position - Vec2::new(4., 2.),
                galley.size() + Vec2::new(8., 4.),
            );
            let painter = ui.painter().with_clip_rect(rect);
            painter.rect_filled(
                bounds,
                ui.visuals().widgets.noninteractive.corner_radius,
                ui.visuals().panel_fill,
            );
            let status = ui.interact(
                bounds.intersect(rect),
                response.id.with("geometry-status"),
                egui::Sense::hover(),
            );
            status.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Label,
                    ui.is_enabled(),
                    &galley.job.text,
                )
            });
            painter.galley(position, galley, ui.visuals().text_color());
            status.on_hover_ui(|ui| {
                ui.label("Untextured source LODs and cable spans share a 48,000-triangle budget. Depth ordering approximates occlusion. Cable previews use static registration order.");
                if let Some(Cables::Unavailable(error)) = cables {
                    ui.label(error);
                }
            });
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub(crate) fn cable_report() -> papyrus_terrain::powerline_wires::Report {
        use papyrus_terrain::powerline_wires::{self, Connection, Node};
        let base = papyrus_terrain::erosion::decode(18000);
        let positions = [[-300., base, 0.], [0., base, 0.], [300., base, 0.]];
        let connections: Vec<_> = positions
            .iter()
            .map(|p| {
                vec![Connection {
                    incoming: [p[0] - 2., p[1] + 140., 0.],
                    outgoing: [p[0] + 2., p[1] + 140., 0.],
                    radius: 0.03,
                    hidden: false,
                }]
            })
            .collect();
        let nodes: Vec<_> = positions
            .iter()
            .enumerate()
            .map(|(i, p)| Node {
                instance: i as u64,
                component: 1,
                root: "fixture",
                wire: "wire",
                position: *p,
                maximum_distance: 400.,
                connections: Some(&connections[i]),
            })
            .collect();
        powerline_wires::assemble(&nodes, Default::default(), || false)
            .unwrap()
            .unwrap()
    }
    #[test]
    fn cable_projection_culls_invalid_near_plane_offscreen_and_degenerate_spans() {
        let rect = Rect::from_min_size(egui::Pos2::ZERO, Vec2::splat(100.));
        let a = (egui::pos2(10., 20.), 0.1);
        let b = (egui::pos2(90., 20.), 0.3);
        let (quad, depth) = cable_quad(a, b, rect).unwrap();
        assert!((quad[0].distance(quad[1]) - 0.8).abs() < 0.00001);
        assert!((depth - 0.2).abs() < 0.00001);
        for invalid in [
            (egui::pos2(f32::NAN, 20.), 0.),
            (egui::pos2(30., 20.), -2.4),
            a,
        ] {
            assert!(cable_quad(a, invalid, rect).is_none());
        }
        assert!(
            cable_quad(
                (egui::pos2(-50., -20.), 0.),
                (egui::pos2(-10., -20.), 0.),
                rect
            )
            .is_none()
        );
    }
    #[test]
    fn cable_geometry_shares_the_existing_triangle_budget_with_models() {
        let mut report = cable_report();
        let span = &report.spans[0];
        report.spans = (0..30_000)
            .map(|_| papyrus_terrain::powerline_wires::Span {
                chain: span.chain,
                start_node: span.start_node,
                end_node: span.end_node,
                connection: span.connection,
                start: span.start,
                end: span.end,
                position: span.position,
                rotation: span.rotation,
                scale: span.scale,
                radius: span.radius,
            })
            .collect();
        let ctx = egui::Context::default();
        let mut camera = Camera::default();
        let mut frame = ctx.run_ui(Default::default(), |ui| {
            let (rect, response) = ui.allocate_exact_size(Vec2::splat(500.), egui::Sense::drag());
            camera.draw_with_scene(
                ui,
                rect,
                &response,
                TextureId::default(),
                &egui::ColorImage::filled([1, 1], Color32::WHITE),
                &vec![18000; 65 * 65],
                65,
                1000,
                &[],
                None,
                Some(Cables::Ready(&report)),
            );
        });
        let mesh = frame
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Mesh(mesh) => Some(mesh),
                _ => None,
            })
            .unwrap();
        assert_eq!(mesh.indices.len(), 64 * 64 * 6 + 48_000 * 3);
        assert_eq!(
            mesh.vertices
                .iter()
                .filter(|v| v.color == Color32::from_rgb(40, 42, 38))
                .count(),
            96_000
        );
        frame.textures_delta.clear();
    }
    fn template() -> papyrus_terrain::path_mesh::Template {
        papyrus_terrain::path_mesh::Template {
            vertices: vec![[-30., 0., -30.], [30., 0., -30.], [0., 0., 30.]],
            normals: vec![[0., 1., 0.]; 3],
            tangents: vec![[1., 0., 0., 1.]; 3],
            uv: vec![[0.; 2]; 3],
            triangles: vec![[0, 1, 2]],
            center: [0.; 3],
            extents: [30., 0., 30.],
        }
    }
    #[test]
    fn surface_preview_borrows_source_buffers_and_keeps_exact_world_bounds() {
        let mut source = template();
        source.validate().unwrap();
        let instance = ModelInstance::surface(&source, [40., 10., -20.], [136., 139., 143.]);
        assert!(std::ptr::eq(
            instance.model.vertices().as_ptr(),
            source.vertices.as_ptr()
        ));
        assert!(std::ptr::eq(
            instance.model.triangles().as_ptr(),
            source.triangles.as_ptr()
        ));
        assert_eq!(instance.world(source.vertices[0]), [10., 10., -50.]);
        assert_eq!(surface_bounds([instance]), vec![[10., -50., 70., 10.]]);
        let mut rotated = ModelInstance::surface(&source, [40., 10., -20.], [136., 139., 143.]);
        rotated.scale = [2., 1., 3.];
        let q = std::f32::consts::FRAC_1_SQRT_2;
        rotated.rotation = [0., q, 0., q];
        for (actual, expected) in surface_bounds([rotated])[0]
            .into_iter()
            .zip([-50., -80., 130., 40.])
        {
            assert!((actual - expected).abs() < 0.0001);
        }
        source.extents[0] = f32::NAN;
        assert!(
            surface_bounds([ModelInstance::surface(&source, [0.; 3], [136., 139., 143.])])
                .is_empty()
        );
    }
    #[test]
    fn surface_models_share_the_bounded_render_batch_and_do_not_modify_source() {
        let ctx = egui::Context::default();
        let mut camera = Camera::default();
        let mut source = template();
        source.triangles = vec![[0, 1, 2]; 540];
        let before = serde_json::to_vec(&source).unwrap();
        let models = (0..200)
            .map(|i| {
                ModelInstance::surface(
                    &source,
                    [(i % 20) as f32 * 5. - 50., 5., (i / 20) as f32 * 5. - 25.],
                    [136., 139., 143.],
                )
            })
            .collect::<Vec<_>>();
        let mut output = ctx.run_ui(Default::default(), |ui| {
            let (rect, response) = ui.allocate_exact_size(Vec2::splat(500.), egui::Sense::drag());
            camera.draw(
                ui,
                rect,
                &response,
                TextureId::default(),
                &egui::ColorImage::new([1, 1], vec![Color32::WHITE]),
                &vec![18000; 65 * 65],
                65,
                1000,
                &models,
            );
        });
        output.textures_delta.clear();
        let mesh = output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Mesh(m) => Some(m),
                _ => None,
            })
            .unwrap();
        let triangles = (mesh.vertices.len() - 65 * 65) / 3;
        assert!(triangles > 0 && triangles <= 48000);
        assert!(triangles < 200 * source.triangles.len());
        assert!(mesh.vertices.iter().all(|v| v.pos.is_finite()));
        assert!(
            mesh.indices
                .iter()
                .all(|i| (*i as usize) < mesh.vertices.len())
        );
        assert_eq!(serde_json::to_vec(&source).unwrap(), before);
    }
    #[test]
    fn degenerate_surface_faces_still_consume_the_frame_work_allowance() {
        let ctx = egui::Context::default();
        let mut camera = Camera::default();
        let mut source = template();
        source.triangles = vec![[0, 0, 0]; 48000];
        let valid = template();
        let models = [
            ModelInstance::surface(&source, [0.; 3], [136., 139., 143.]),
            ModelInstance::surface(&valid, [0.; 3], [136., 139., 143.]),
        ];
        let mut output = ctx.run_ui(Default::default(), |ui| {
            let (rect, response) = ui.allocate_exact_size(Vec2::splat(500.), egui::Sense::drag());
            camera.draw(
                ui,
                rect,
                &response,
                TextureId::default(),
                &egui::ColorImage::new([1, 1], vec![Color32::WHITE]),
                &vec![18000; 65 * 65],
                65,
                1000,
                &models,
            );
        });
        output.textures_delta.clear();
        let mesh = output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Mesh(m) => Some(m),
                _ => None,
            })
            .unwrap();
        assert_eq!(mesh.vertices.len(), 65 * 65);
    }
    #[test]
    fn terrain_detail_reuses_samples_and_rebuilds_for_the_next_composed_map() {
        use papyrus_terrain::{erosion::decode, native_layers};

        let size = 1000;
        let (hr, sr) = native_layers::dimensions(size).unwrap();
        let mut layers = native_layers::Layers {
            size,
            terrain_seed: 1,
            biome_seed: 2,
            height_resolution: hr,
            surface_resolution: sr,
            height: vec![18000; hr * hr],
            biome: vec![],
            splat: vec![],
            topology: vec![],
            normalized_biome_cells: 0,
            normalized_splat_cells: 0,
        };
        let mut camera = Camera::default();
        let resolution = 2;
        let fallback = [19000; 4];
        let ground = NativeGround {
            revision: TextureId::Managed(1),
            layers: &layers,
            surfaces: &[[-80., -80., 80., 80.]],
        };
        let detail = camera.terrain_detail(Some(ground), &fallback, resolution, size);
        let samples = detail.native_heights.as_ref().unwrap();
        assert!(detail.grid.vertices.len() > 65 * 65);
        assert_eq!(samples.len(), detail.grid.vertices.len());
        for (&[u, v], height) in detail.grid.vertices.iter().zip(samples) {
            assert_eq!(*height, papyrus_terrain::spawn::sample_height(&layers, u, 1. - v).unwrap());
        }
        let samples_ptr = samples.as_ptr();
        camera.reset();
        let detail = camera.terrain_detail(Some(ground), &fallback, resolution, size);
        assert_eq!(samples_ptr, detail.native_heights.as_ref().unwrap().as_ptr());

        layers.height.fill(20000);
        layers.height[hr * (hr - 1)] = -1;
        let next = NativeGround {
            revision: TextureId::Managed(2),
            layers: &layers,
            surfaces: &[],
        };
        let detail = camera.terrain_detail(Some(next), &fallback, resolution, size);
        assert_eq!(detail.grid.vertices.len(), 65 * 65);
        let samples = detail.native_heights.as_ref().unwrap();
        assert_eq!(samples[0], decode(19000));
        assert_eq!(*samples.last().unwrap(), decode(20000));
        assert!(camera.terrain_detail(None, &fallback, resolution, size).native_heights.is_none());
    }

    #[test]
    fn indexed_surface_coverage_keeps_the_exact_refined_mesh() {
        // Many small regions plus clipped or collapsed corners. Collapsed
        // coordinates inside a cell are distinct from those on its edge.
        let mut rects = vec![
            [0, 0, 0, 1024],
            [3, 19, 3, 501],
            [512, 1024, 700, 1024],
            [1021, 1021, 1024, 1024],
            [600, 300, 980, 301],
        ];
        rects.extend((0..180).map(|index| {
            let x = (index * 71) % 980;
            let z = (index * 131) % 990;
            [x, z, x + 1 + index % 31, z + index % 23]
        }));
        let expected = terrain_grid_refined(|[x, z, r, b]| {
            rects.iter().any(|q| x < q[2] && r > q[0] && z < q[3] && b > q[1])
        });
        let actual = terrain_grid(&rects);
        assert!(actual.vertices == expected.vertices, "Refined vertices changed");
        assert!(actual.triangles == expected.triangles, "Refined triangles changed");
    }

    #[test]
    fn local_terrain_detail_is_bounded_covers_the_map_and_stitches_neighbor_edges() {
        let legacy = terrain_grid(&[]);
        assert_eq!(legacy.vertices.len(), 65 * 65);
        assert_eq!(legacy.triangles.len(), 64 * 64 * 2);
        for rects in [
            vec![[400, 400, 640, 640]],
            vec![[0, 0, 1024, 1024]],
            vec![[0, 480, 1024, 550], [0, 0, 70, 1024]],
        ] {
            let grid = terrain_grid(&rects);
            assert!(grid.triangles.len() < 100000);
            assert!(grid.vertices.len() < 120000);
            let mut area = 0.;
            let mut edges = std::collections::BTreeMap::new();
            for t in &grid.triangles {
                let [a, b, c] = t.map(|i| grid.vertices[i as usize]);
                let cross = (b[0] as f64 - a[0] as f64) * (c[1] as f64 - a[1] as f64)
                    - (b[1] as f64 - a[1] as f64) * (c[0] as f64 - a[0] as f64);
                assert!(cross > 0.);
                area += cross * 0.5;
                let p = [a, b, c]
                    .map(|p| [(p[0] * 2048.).round() as u16, (p[1] * 2048.).round() as u16]);
                for (a, b) in [(p[0], p[1]), (p[1], p[2]), (p[2], p[0])] {
                    let edge = if a < b { (a, b) } else { (b, a) };
                    *edges.entry(edge).or_insert(0) += 1;
                }
            }
            assert!((area - 1.).abs() < 1e-9);
            for ((a, b), count) in edges {
                let boundary = (a[0] == 0 && b[0] == 0)
                    || (a[0] == 2048 && b[0] == 2048)
                    || (a[1] == 0 && b[1] == 0)
                    || (a[1] == 2048 && b[1] == 2048);
                assert_eq!(count, if boundary { 1 } else { 2 });
            }
        }
    }
    fn camera_frame(ctx: &egui::Context, camera: &mut Camera, events: Vec<egui::Event>) {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::splat(600.))),
                events,
                focused: true,
                ..Default::default()
            },
            |ui| {
                let (rect, response) =
                    ui.allocate_exact_size(Vec2::splat(500.), egui::Sense::click_and_drag());
                camera.draw(
                    ui,
                    rect,
                    &response,
                    TextureId::default(),
                    &egui::ColorImage::new([1, 1], vec![Color32::WHITE]),
                    &vec![18000; 65 * 65],
                    65,
                    4000,
                    &[],
                );
            },
        );
        output.textures_delta.clear();
    }
    #[test]
    fn quick_look_retains_motion_when_move_and_release_share_a_frame() {
        let ctx = egui::Context::default();
        let mut camera = Camera::default();
        let start = egui::pos2(200., 200.);
        let end = egui::pos2(300., 250.);
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        camera_frame(&ctx, &mut camera, vec![]);
        camera_frame(
            &ctx,
            &mut camera,
            vec![egui::Event::PointerMoved(start), button(start, true)],
        );
        let before = camera.yaw;
        camera_frame(
            &ctx,
            &mut camera,
            vec![egui::Event::PointerMoved(end), button(end, false)],
        );
        assert!((camera.yaw - before).abs() > 0.3);
        let after = camera.yaw;
        camera_frame(
            &ctx,
            &mut camera,
            vec![egui::Event::PointerMoved(end + egui::vec2(20., 0.))],
        );
        assert_eq!(camera.yaw, after);
    }
    #[test]
    fn fly_uses_relative_motion_and_releases_capture_on_escape_or_hidden_view() {
        use egui::{Event, Key, Modifiers, PointerButton};

        let ctx = egui::Context::default();
        let mut camera = Camera::default();
        let pos = egui::pos2(200., 200.);
        let press = Event::PointerButton {
            pos, button: PointerButton::Secondary, pressed: true, modifiers: Modifiers::NONE,
        };
        let key = |key, pressed| Event::Key {
            key, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE,
        };
        camera_frame(&ctx, &mut camera, vec![]);
        let eye = camera.eye;
        camera_frame(&ctx, &mut camera, vec![key(Key::W, true)]);
        assert_eq!(camera.eye, eye);
        camera_frame(&ctx, &mut camera, vec![Event::PointerMoved(pos), press.clone()]);
        assert!(camera.captured);
        assert_ne!(camera.eye, eye);

        // A stationary screen cursor still receives raw motion while grabbed.
        for _ in 0..4 {
            let yaw = camera.yaw;
            camera_frame(&ctx, &mut camera, vec![Event::MouseMoved(egui::vec2(650., 0.))]);
            assert_ne!(camera.yaw, yaw);
        }
        let yaw = camera.yaw;
        camera_frame(&ctx, &mut camera, vec![Event::PointerMoved(pos + egui::vec2(200., 0.))]);
        assert_eq!(camera.yaw, yaw);
        camera_frame(&ctx, &mut camera, vec![key(Key::Escape, true), key(Key::W, false)]);
        assert!(!camera.captured);
        camera_frame(&ctx, &mut camera, vec![Event::MouseMoved(egui::vec2(200., 0.))]);
        assert_eq!(camera.yaw, yaw);

        camera_frame(&ctx, &mut camera, vec![key(Key::Escape, false), press]);
        assert!(camera.captured);
        camera.end_frame(&ctx);
        camera.end_frame(&ctx);
        assert!(!camera.captured);
    }

    #[test]
    fn live_scene_draws_coarse_terrain_before_the_worker_finishes() {
        let ctx = egui::Context::default();
        let mut camera = Camera::default();
        let image = Arc::new(egui::ColorImage::filled([2, 2], Color32::BLUE));
        let heights = vec![18000; 65 * 65];
        let texture = TextureId::User(1);
        let frame = |camera: &mut Camera| {
            let mut refining = false;
            let mut output = ctx.run_ui(Default::default(), |ui| {
                let (rect, response) = ui.allocate_exact_size(Vec2::splat(500.), egui::Sense::drag());
                refining = camera.draw_live(ui, rect, &response, texture, &image,
                    &heights, 65, 4000, &[], None, None);
            });
            let mesh_texture = output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Mesh(mesh) if !mesh.indices.is_empty() => Some(mesh.texture_id),
                _ => None,
            }).unwrap();
            output.textures_delta.clear();
            (refining, mesh_texture)
        };
        assert!(camera.details_task.is_none());
        assert_eq!(frame(&mut camera), (true, texture));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let (refining, ready_texture) = frame(&mut camera);
            if !refining {
                assert_ne!(ready_texture, texture);
                assert!(camera.details_task.is_none());
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
    }

    #[test]
    fn near_plane_clips_crossing_faces_and_interpolates_their_uvs() {
        let front = ViewVertex { point: egui::vec2(1., 1.), depth: 2. * NEAR, uv: egui::pos2(1., 1.) };
        let other = ViewVertex { point: egui::vec2(1., 0.), uv: egui::pos2(1., 0.), ..front };
        let behind = ViewVertex { point: Vec2::ZERO, depth: 0., uv: egui::Pos2::ZERO };
        let (polygon, count) = clip_triangle([front, other, behind]);
        assert_eq!(count, 4);
        assert!(polygon.iter().all(|vertex| vertex.depth >= NEAR));
        assert_eq!(polygon[0].uv, egui::pos2(0.5, 0.5));
        assert_eq!(polygon[3].uv, egui::pos2(0.5, 0.));
        assert_eq!(clip_triangle([behind; 3]).1, 0);
    }

    #[test]
    fn quick_pan_tracks_release_and_outside_presses_cannot_capture_the_camera() {
        let ctx = egui::Context::default();
        let mut camera = Camera::default();
        let start = egui::pos2(200., 200.);
        let end = egui::pos2(300., 250.);
        let button = |pos, button, pressed| egui::Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        camera_frame(&ctx, &mut camera, vec![]);
        camera_frame(
            &ctx,
            &mut camera,
            vec![
                egui::Event::PointerMoved(start),
                button(start, egui::PointerButton::Middle, true),
            ],
        );
        camera_frame(
            &ctx,
            &mut camera,
            vec![
                egui::Event::PointerMoved(end),
                button(end, egui::PointerButton::Middle, false),
            ],
        );
        assert_ne!(camera.eye, Camera::default().eye);
        assert!(camera.pointer_drag.is_none());
        let before = camera.yaw;
        let outside = egui::pos2(550., 550.);
        camera_frame(
            &ctx,
            &mut camera,
            vec![
                egui::Event::PointerMoved(outside),
                button(outside, egui::PointerButton::Primary, true),
            ],
        );
        camera_frame(
            &ctx,
            &mut camera,
            vec![
                egui::Event::PointerMoved(end),
                button(end, egui::PointerButton::Primary, false),
            ],
        );
        assert_eq!(camera.yaw, before);
        assert!(camera.pointer_drag.is_none());
    }
    #[test]
    fn full_pose_matches_hierarchy_math_and_preserves_monument_yaw_convention() {
        let model = mapper_studio::monument_source::Model {
            vertices: vec![],
            triangles: vec![],
            issues: vec![],
            complete: false,
            simplified: false,
        };
        let mut instance = ModelInstance::yaw(&model, [1., 2., 3.], 37.);
        let (s, c) = 37_f32.to_radians().sin_cos();
        let actual = instance.world([1., 2., 3.]);
        for (actual, expected) in actual
            .into_iter()
            .zip([1. + c - 3. * s, 4., 3. + s + 3. * c])
        {
            assert!((actual - expected).abs() < 1e-5);
        }
        let q = std::f32::consts::FRAC_1_SQRT_2;
        instance.rotation = [q, 0., 0., q];
        instance.scale = [2., 3., 4.];
        for (actual, expected) in instance.world([1., 2., 3.]).into_iter().zip([3., -10., 9.]) {
            assert!((actual - expected).abs() < 1e-5);
        }
        instance.scale[0] = 0.;
        assert!(!instance.valid());
    }
    #[test]
    fn visible_models_after_the_first_128_are_not_starved_by_offscreen_instances() {
        let ctx = egui::Context::default();
        let mut camera = Camera::default();
        let model = mapper_studio::monument_source::Model {
            vertices: vec![[-30., 0., 0.], [30., 0., 0.], [0., 60., 0.]],
            triangles: vec![[0, 1, 2]],
            issues: vec![],
            complete: true,
            simplified: false,
        };
        let mut models = (0..128)
            .map(|_| ModelInstance::yaw(&model, [100000., 0., 0.], 0.))
            .collect::<Vec<_>>();
        models.push(ModelInstance::yaw(&model, [0., 100., 0.], 0.));
        let mut output = ctx.run_ui(Default::default(), |ui| {
            let (rect, response) = ui.allocate_exact_size(Vec2::splat(500.), egui::Sense::drag());
            camera.draw(
                ui,
                rect,
                &response,
                TextureId::default(),
                &egui::ColorImage::new([1, 1], vec![Color32::WHITE]),
                &vec![18000; 65 * 65],
                65,
                4000,
                &models,
            );
        });
        let mesh = output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Mesh(m) => Some(m),
                _ => None,
            })
            .unwrap();
        assert_eq!(mesh.vertices.len(), 65 * 65 + 3);
        assert!(mesh.indices.len() <= 64 * 64 * 6 + 48000 * 3);
        output.textures_delta.clear();
    }
    #[test]
    fn bounded_mesh_renders_without_changing_recipe() {
        let ctx = egui::Context::default();
        let mut camera = Camera {
            enabled: true,
            ..Default::default()
        };
        let mut output = ctx.run_ui(Default::default(), |ui| {
            let (rect, response) = ui.allocate_exact_size(Vec2::splat(500.), egui::Sense::drag());
            camera.draw(
                ui,
                rect,
                &response,
                TextureId::default(),
                &egui::ColorImage::new([1, 1], vec![Color32::WHITE]),
                &vec![18000; 65 * 65],
                65,
                4000,
                &[],
            );
        });
        let mesh = output
            .shapes
            .iter()
            .find_map(|s| {
                if let egui::Shape::Mesh(m) = &s.shape {
                    Some(m)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(mesh.vertices.len(), 65 * 65);
        assert_eq!(mesh.indices.len(), 64 * 64 * 6);
        assert!(mesh.vertices.iter().all(|v| v.pos.is_finite()));
        camera.reset();
        assert!(camera.enabled);
        output.textures_delta.clear();
    }
    #[test]
    #[ignore = "requires locally extracted vine preview models"]
    fn owned_vine_models_project_through_the_shared_renderer() {
        let path = std::env::var_os("MAPPER_VINE_PREVIEW_MODELS").expect("model document");
        let document: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let models: std::collections::BTreeMap<String, mapper_studio::monument_source::Model> =
            serde_json::from_value(document["models"].clone()).unwrap();
        assert_eq!(models.len(), 3);
        let mut projections = 0;
        for light in [false, true] {
            for model in models.values() {
                assert!(model.complete && model.issues.is_empty());
                let ctx = egui::Context::default();
                ctx.set_visuals(if light { egui::Visuals::light() } else { egui::Visuals::dark() });
                let mut camera = Camera::default();
                camera.translate(1.32, 0., 0.);
                let mut output = ctx.run_ui(Default::default(), |ui| {
                    let (rect, response) = ui.allocate_exact_size(Vec2::splat(500.), egui::Sense::drag());
                    camera.draw(ui, rect, &response, TextureId::default(),
                        &egui::ColorImage::new([2, 2], vec![Color32::BLUE; 4]),
                        &vec![18000; 65 * 65], 65, 4000,
                        &[ModelInstance::yaw(model, [0., papyrus_terrain::erosion::decode(18000), 0.], 37.)],
                    );
                });
                let mesh = output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Mesh(mesh) => Some(mesh), _ => None,
                }).unwrap();
                assert!(mesh.vertices.len() > 65 * 65);
                assert!(mesh.indices.len() > 64 * 64 * 6);
                assert!(mesh.vertices.iter().all(|vertex| vertex.pos.is_finite()));
                assert!(mesh.vertices[65 * 65..].iter().all(|vertex| vertex.uv.y > 2. / 3.));
                output.textures_delta.clear();
                projections += 1;
            }
        }
        assert_eq!(projections, 6);
    }
    #[test]
    fn static_model_uses_atlas_white_row_and_finite_projected_geometry() {
        let ctx = egui::Context::default();
        let mut camera = Camera::default();
        let model = mapper_studio::monument_source::Model {
            vertices: vec![[-20., 0., 0.], [20., 0., 0.], [0., 40., 0.]],
            triangles: vec![[0, 1, 2]],
            issues: vec![],
            complete: true,
            simplified: false,
        };
        let mut output = ctx.run_ui(Default::default(), |ui| {
            let (rect, response) = ui.allocate_exact_size(Vec2::splat(500.), egui::Sense::drag());
            camera.draw(
                ui,
                rect,
                &response,
                TextureId::default(),
                &egui::ColorImage::new([2, 2], vec![Color32::BLUE; 4]),
                &vec![18000; 65 * 65],
                65,
                4000,
                &[ModelInstance::yaw(
                    &model,
                    [0., papyrus_terrain::erosion::decode(18000), 0.],
                    37.,
                )],
            );
        });
        let mesh = output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Mesh(m) => Some(m),
                _ => None,
            })
            .unwrap();
        assert_eq!(mesh.vertices.len(), 65 * 65 + 3);
        assert_eq!(mesh.indices.len(), 64 * 64 * 6 + 3);
        assert!(mesh.vertices.iter().all(|v| v.pos.is_finite()));
        assert!(mesh.vertices[65 * 65..].iter().all(|v| v.uv.y > 2. / 3.));
        output.textures_delta.clear();
    }
}
