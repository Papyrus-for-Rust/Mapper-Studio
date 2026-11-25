use super::*;
use mapper_studio::path_edit::{Change, Key, Operation, Route, changed_segments};
use std::collections::{HashMap, VecDeque};
use std::ops::Range;

const SEGMENTS_PER_BLOCK: usize = 32;
const PICK_RADIUS: f32 = 8.;
const CONTROL_PICK_RADIUS: f32 = 7.;
const CONTROL_SPACING: f32 = 56.;
const DIRECT_BEND_RADIUS: f32 = 18.;
const CACHED_CURVES: usize = 16;
const CACHED_CURVE_POINTS: usize = 32_768;
const CURVE_SUBDIVISIONS: usize = 4;

#[derive(Default)]
pub(super) struct Draft {
    changes: Vec<Change>,
    routes: HashMap<Key, Route>,
    selected: Option<Key>,
    control: Option<usize>,
    drag: Option<Drag>,
    menu: Option<PointMenu>,
    bounds: HashMap<Key, RouteBounds>,
    changed_segments: HashMap<Key, Vec<Range<usize>>>,
    curves: CurveCache,
}

struct RouteBounds {
    whole: Rect,
    blocks: Vec<Rect>,
    curve_padding: f32,
}

impl RouteBounds {
    fn new(controls: &[[f32; 2]]) -> Self {
        let mut whole = Rect::NOTHING;
        let blocks = (0..controls.len().saturating_sub(1))
            .step_by(SEGMENTS_PER_BLOCK)
            .map(|start| {
                let end = (start + SEGMENTS_PER_BLOCK).min(controls.len() - 1);
                let mut bounds = Rect::NOTHING;
                for &control in &controls[start..=end] {
                    bounds.extend_with(control.into());
                }
                whole = whole.union(bounds);
                bounds
            })
            .collect();
        Self { whole, blocks, curve_padding: 0. }
    }

    fn for_controls(controls: &[[f32; 2]]) -> Self {
        let mut bounds = Self::new(controls);
        let length: f32 = controls
            .windows(2)
            .map(|p| (Vec2::from(p[1]) - Vec2::from(p[0])).length())
            .sum();
        // The two unit tangents contribute at most t*(1-t) times the
        // average control spacing to the Hermite curve's position.
        bounds.curve_padding = length / controls.len() as f32 * 0.25;
        bounds
    }
}

#[derive(Default)]
struct CurveCache {
    entries: VecDeque<(Key, Curve)>,
    points: usize,
}

struct Curve {
    points: Vec<[f32; 2]>,
    bounds: RouteBounds,
}

fn fits_curve_cache(route: &Route) -> bool {
    (route.controls.len() - 1) * CURVE_SUBDIVISIONS < CACHED_CURVE_POINTS
}

impl CurveCache {
    fn remove(&mut self, key: Key) {
        if let Some(index) = self.entries.iter().position(|(cached, _)| *cached == key) {
            self.points -= self.entries.remove(index).unwrap().1.points.len();
        }
    }

    fn curve(&mut self, route: &Route) -> &Curve {
        let cached = self.entries.iter().position(|(key, _)| *key == route.key);
        if let Some(index) = cached {
            let entry = self.entries.remove(index).unwrap();
            self.entries.push_back(entry);
        } else {
            let points = sample_curve(route, None);
            while self.entries.len() == CACHED_CURVES
                || self.points + points.len() > CACHED_CURVE_POINTS
            {
                self.points -= self.entries.pop_front().unwrap().1.points.len();
            }
            self.points += points.len();
            let bounds = RouteBounds::new(&points);
            self.entries.push_back((route.key, Curve { points, bounds }));
        }
        &self.entries.back().unwrap().1
    }

    fn line(&mut self, route: &Route, drag: Option<Drag>, map: Rect, size: u32) -> Vec<Pos2> {
        let project = |p| screen(map, size, p);
        if !fits_curve_cache(route) {
            return preview_controls(route, drag).map(project).collect();
        }
        if drag.is_some() {
            return sample_curve(route, drag)
                .into_iter()
                .map(project)
                .collect();
        }
        self.curve(route)
            .points
            .iter()
            .copied()
            .map(project)
            .collect()
    }
}

#[derive(Clone, Copy)]
struct Drag {
    route: Key,
    control: usize,
    press: Pos2,
    origin: [f32; 2],
    next: [f32; 2],
    moved: bool,
    fixed: bool,
    on_curve: bool,
    change: Change,
}

struct PointMenu {
    route: Key,
    insert: Option<Change>,
    remove: Option<Change>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum MonumentPress {
    None,
    Footprint,
    Marker,
}

impl Draft {
    pub(super) fn changes(&self) -> &[Change] {
        &self.changes
    }

    pub(super) fn restore(
        routes: &[Route],
        changes: Vec<Change>,
        size: u32,
    ) -> Result<Self, String> {
        mapper_studio::path_edit::validate(&changes, size)?;
        let mut draft = Self::default();
        for &change in &changes {
            draft.apply_change(routes, change)?;
        }
        draft.changes = changes;
        Ok(draft)
    }

    pub(super) fn record(
        &mut self,
        routes: &[Route],
        change: Change,
        size: u32,
    ) -> Result<(), String> {
        mapper_studio::path_edit::validate_append(self.changes.len(), &change, size)?;
        self.apply_change(routes, change)?;
        self.changes.push(change);
        self.bounds.remove(&change.route);
        self.changed_segments.remove(&change.route);
        self.curves.remove(change.route);
        Ok(())
    }

    fn apply_change(&mut self, routes: &[Route], change: Change) -> Result<(), String> {
        match self.routes.entry(change.route) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                entry.get_mut().apply(change)?
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                let mut route = routes
                    .iter()
                    .find(|r| r.key == change.route)
                    .ok_or("The selected spline is missing")?
                    .clone();
                route.apply(change)?;
                entry.insert(route);
            }
        }
        Ok(())
    }

    pub(super) fn step_history(
        &mut self,
        routes: &[Route],
        change: Change,
        size: u32,
        redo: bool,
    ) -> Result<(), String> {
        if redo {
            self.record(routes, change, size)?;
        } else {
            self.undo_last(routes, change)?;
        }
        self.control = None;
        Ok(())
    }

    fn undo_last(&mut self, routes: &[Route], change: Change) -> Result<(), String> {
        let (last, earlier) = self
            .changes
            .split_last()
            .ok_or("No spline change to undo")?;
        if *last != change {
            return Err("The spline draft has changed".into());
        }
        let mut remaining = earlier
            .iter()
            .copied()
            .filter(|item| item.route == change.route);
        if let Some(first) = remaining.next() {
            let mut route = routes
                .iter()
                .find(|route| route.key == change.route)
                .ok_or("The selected spline is missing")?
                .clone();
            route.apply(first)?;
            for edit in remaining {
                route.apply(edit)?;
            }
            self.routes.insert(change.route, route);
        } else {
            self.routes.remove(&change.route);
        }
        self.changes.pop();
        self.bounds.remove(&change.route);
        self.changed_segments.remove(&change.route);
        self.curves.remove(change.route);
        Ok(())
    }

    fn displayed<'a>(&'a self, base: &'a Route) -> &'a Route {
        self.routes.get(&base.key).unwrap_or(base)
    }

    fn control_at(
        &self, route: &Route, map: Rect, size: u32, pointer: Pos2,
    ) -> Option<usize> {
        visible_controls(route, map, size, self.control)
            .map(|index| (index, pointer.distance_sq(screen(map, size, route.controls[index]))))
            .filter(|&(_, distance)| distance <= CONTROL_PICK_RADIUS.powi(2))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(index, _)| index)
    }

    pub(super) fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub(super) fn cancel_gesture(&mut self) {
        self.drag = None;
        self.menu = None;
    }

    fn world(map: Rect, size: u32, point: Pos2) -> [f32; 2] {
        let size = size as f32;
        [
            ((point.x - map.left()) / map.width() - 0.5).clamp(-0.5, 0.5) * size,
            (0.5 - (point.y - map.top()) / map.height()).clamp(-0.5, 0.5) * size,
        ]
    }

    fn nearest(
        &mut self,
        routes: &[Route],
        map: Rect,
        size: u32,
        pointer: Pos2,
    ) -> Option<(f32, Key, usize, Pos2)> {
        let mut nearest: Option<(f32, Key, usize, Pos2)> = None;
        for base in routes {
            let route = self.routes.get(&base.key).unwrap_or(base);
            let overlaps = |bounds: Rect| {
                let rect = Rect::from_two_pos(
                    screen(map, size, bounds.min.into()),
                    screen(map, size, bounds.max.into()),
                );
                rect.expand(PICK_RADIUS + 0.01).contains(pointer)
            };
            let base = self
                .bounds
                .entry(route.key)
                .or_insert_with(|| RouteBounds::for_controls(&route.controls));
            let (points, bounds, subdivisions) = if fits_curve_cache(route) {
                if !overlaps(base.whole.expand(base.curve_padding))
                    || !base
                        .blocks
                        .iter()
                        .any(|block| overlaps(block.expand(base.curve_padding)))
                {
                    continue;
                }
                let curve = self.curves.curve(route);
                let subdivided = curve.points.len() == (route.controls.len() - 1) * CURVE_SUBDIVISIONS + 1;
                (
                    curve.points.as_slice(),
                    &curve.bounds,
                    if subdivided { CURVE_SUBDIVISIONS } else { 1 },
                )
            } else {
                (route.controls.as_slice(), &*base, 1)
            };
            if bounds.blocks.len() > 1 && !overlaps(bounds.whole) {
                continue;
            }
            for (block, &bounds) in bounds.blocks.iter().enumerate() {
                if !overlaps(bounds) {
                    continue;
                }
                let start = block * SEGMENTS_PER_BLOCK;
                let end = (start + SEGMENTS_PER_BLOCK).min(points.len() - 1);
                for (i, segment) in points[start..=end].windows(2).enumerate() {
                    let (distance, projection) = project_segment(map, size, pointer, segment);
                    if distance <= PICK_RADIUS * PICK_RADIUS
                        && nearest.as_ref().is_none_or(|(d, ..)| distance < *d)
                    {
                        nearest = Some((distance, route.key, (start + i) / subdivisions, projection));
                    }
                }
            }
        }
        nearest
    }

    fn insertion(route: &Route, control: usize, point: [f32; 2]) -> Option<Change> {
        let spaced = |other: [f32; 2]| (Vec2::from(point) - Vec2::from(other)).length_sq() >= 1.;
        (spaced(route.controls[control]) && spaced(route.controls[control + 1])).then_some(Change {
            route: route.key,
            control,
            expected: route.controls[control],
            operation: Operation::Insert(point),
        })
    }

    fn removal(route: &Route, control: usize) -> Option<Change> {
        (!route.is_fixed(control)).then_some(Change {
            route: route.key,
            control,
            expected: route.controls[control],
            operation: Operation::Remove,
        })
    }

    fn point_menu(
        &mut self,
        response: &egui::Response,
        routes: &[Route],
        map: Rect,
        size: u32,
    ) -> Option<Change> {
        if response.secondary_clicked() {
            self.menu = response.interact_pointer_pos().and_then(|pos| {
                let selected_control = self.selected
                    .and_then(|key| routes.iter().find(|r| r.key == key))
                    .map(|base| self.displayed(base))
                    .and_then(|route| self.control_at(route, map, size, pos).map(|_| route.key));
                let hit = self.nearest(routes, map, size, pos);
                let key = selected_control.or_else(|| hit.map(|(_, key, ..)| key))?;
                let route = self.displayed(routes.iter().find(|r| r.key == key)?);
                let insert = hit.filter(|(_, hit_key, ..)| *hit_key == key)
                    .and_then(|(_, _, i, point)| Self::insertion(route, i, Self::world(map, size, point)));
                let remove = self.control_at(route, map, size, pos)
                    .and_then(|i| Self::removal(route, i));
                Some(PointMenu { route: key, insert, remove })
            });
        }
        let menu = self.menu.as_ref()?;
        let mut change = None;
        response.context_menu(|ui| {
            ui.strong(menu.route.kind.label());
            if ui.add_enabled(menu.insert.is_some(), egui::Button::new("Add point")).clicked() {
                change = menu.insert;
                ui.close();
            }
            if ui.add_enabled(menu.remove.is_some(), egui::Button::new("Remove point")).clicked() {
                change = menu.remove;
                ui.close();
            }
        });
        if !response.context_menu_opened() {
            self.menu = None;
        }
        change
    }

    pub(super) fn input(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        routes: &[Route],
        map: Rect,
        size: u32,
        monument_press: MonumentPress,
    ) -> (bool, Result<bool, String>) {
        let primary = egui::PointerButton::Primary;
        let (press, release, down, cancel) = ui.input(|input| {
            let mut press = None;
            let mut release = None;
            let mut cancel = !input.focused || input.key_pressed(egui::Key::Escape);
            for event in &input.raw.events {
                match event {
                    egui::Event::PointerButton {
                        pos,
                        button,
                        pressed,
                        ..
                    } if *button == primary => {
                        if *pressed {
                            press = Some(*pos);
                        } else {
                            release = Some(*pos);
                        }
                    }
                    egui::Event::PointerGone => cancel = true,
                    _ => {}
                }
            }
            (press, release, input.pointer.button_down(primary), cancel)
        });
        if cancel {
            let owned = self.drag.take().is_some();
            self.menu = None;
            self.selected = None;
            self.control = None;
            return (owned, Ok(false));
        }
        if self.drag.is_none()
            && let Some(change) = self.point_menu(response, routes, map, size)
        {
            let applied = self.record(routes, change, size).map(|()| true);
            if applied.is_ok() {
                self.selected = Some(change.route);
                self.control = match change.operation {
                    Operation::Insert(_) => Some(change.control + 1),
                    _ => None,
                };
                response.request_focus();
            }
            return (true, applied);
        }
        let selected = self
            .selected
            .and_then(|key| routes.iter().find(|r| r.key == key));
        if self.drag.is_none()
            && monument_press != MonumentPress::Marker
            && let Some(pos) = press.filter(|pos| {
                response.interact_rect.contains(*pos)
                    && ui.ctx().layer_id_at(*pos) == Some(ui.layer_id())
            })
            && let Some(base) = selected
        {
            let handle = self.control_at(self.displayed(base), map, size, pos);
            let control = handle.or_else(|| {
                if monument_press != MonumentPress::None {
                    return None;
                }
                let (_, key, segment, _) = self.nearest(routes, map, size, pos)?;
                if key != base.key {
                    return None;
                }
                bend_control(self.displayed(base), segment, map, size, pos)
            });
            if let Some(control) = control {
                let route = self.displayed(base);
                let origin = route.controls[control];
                self.drag = Some(Drag {
                    route: route.key,
                    control,
                    press: pos,
                    origin,
                    next: origin,
                    moved: false,
                    fixed: route.is_fixed(control),
                    on_curve: handle.is_none(),
                    change: route.drag_change(control, origin),
                });
                if handle.is_some() {
                    self.control = Some(control);
                }
                response.request_focus();
            }
        }
        if let Some(mut drag) = self.drag {
            if !drag.fixed && let Some(pos) = release.or(response.interact_pointer_pos()) {
                let threshold = ui.ctx().options(|options| options.input_options.max_click_dist);
                drag.moved |= pos.distance_sq(drag.press) > threshold * threshold;
                let next = if !drag.moved || pos == drag.press {
                    drag.origin
                } else {
                    Self::world(
                        map,
                        size,
                        screen(map, size, drag.origin) + (pos - drag.press),
                    )
                };
                if next != drag.next && let Some(base) = selected {
                    drag.next = next;
                    drag.change = self.displayed(base).drag_change(drag.control, next);
                }
            }
            self.drag = down.then_some(drag);
            if drag.moved {
                self.control = Some(drag.control);
            }
            if !down && release.is_some() && drag.next != drag.origin {
                return (
                    true,
                    self.record(
                        routes,
                        drag.change,
                        size,
                    )
                    .map(|()| true),
                );
            }
            // A click on the curve still participates in double-click insertion.
            if down || !drag.on_curve || drag.moved || release.is_none() {
                return (true, Ok(false));
            }
        }
        if monument_press == MonumentPress::Marker {
            if press.is_some() {
                self.selected = None;
                self.control = None;
            }
            return (false, Ok(false));
        }
        if response.clicked_by(primary) {
            let nearest = response
                .interact_pointer_pos()
                .and_then(|pos| self.nearest(routes, map, size, pos));
            if let Some((_, key, control, projected)) = nearest {
                if self.selected == Some(key) && response.double_clicked_by(primary) {
                    let route = self.displayed(routes.iter().find(|r| r.key == key).unwrap());
                    if let Some(change) =
                        Self::insertion(route, control, Self::world(map, size, projected))
                    {
                        let applied = self.record(routes, change, size).map(|()| true);
                        if applied.is_ok() {
                            self.control = Some(control + 1);
                        }
                        return (true, applied);
                    }
                }
                if self.selected != Some(key) {
                    self.control = None;
                }
                self.selected = Some(key);
                response.request_focus();
            } else {
                self.selected = None;
                self.control = None;
            }
        }
        if response.has_focus()
            && ui.input(|input| input.key_pressed(egui::Key::Delete))
            && let Some(point) = self.control
            && let Some(route) = self
                .selected
                .and_then(|key| routes.iter().find(|r| r.key == key))
        {
            let route = self.displayed(route);
            let Some(change) = Self::removal(route, point) else {
                return (true, Ok(false));
            };
            let applied = self.record(routes, change, size).map(|()| true);
            if applied.is_ok() {
                self.control = None;
            }
            return (true, applied);
        }
        (response.clicked_by(primary) && self.selected.is_some(), Ok(false))
    }

    pub(super) fn hover(
        &mut self,
        routes: &[Route],
        map: Rect,
        size: u32,
        response: &egui::Response,
    ) -> Option<Key> {
        let pointer = response.hover_pos().filter(|_| self.drag.is_none());
        let control = pointer.and_then(|pos| {
            let route = self.displayed(routes.iter().find(|route| Some(route.key) == self.selected)?);
            self.control_at(route, map, size, pos).map(|index| (route.key, route.is_fixed(index)))
        });
        let hover = control.map(|(key, _)| key).or_else(|| {
            pointer.and_then(|pos| self.nearest(routes, map, size, pos)).map(|(_, key, ..)| key)
        });
        if let Some(key) = hover {
            let (hint, cursor) = match control {
                Some((_, true)) => ("Anchored point", egui::CursorIcon::Default),
                Some((_, false)) => ("Drag to bend · Right-click for point actions", egui::CursorIcon::Grab),
                None if self.selected == Some(key) => ("Drag to bend · Double-click to add a point", egui::CursorIcon::Crosshair),
                None => ("Click to select", egui::CursorIcon::PointingHand),
            };
            response
                .clone()
                .on_hover_cursor(cursor)
                .on_hover_text_at_pointer(format!("{} · {hint}", key.kind.label()));
        }
        hover
    }

    pub(super) fn paint(
        &mut self,
        painter: &egui::Painter,
        routes: &[Route],
        map: Rect,
        size: u32,
        hover: Option<Key>,
        editable: bool,
    ) {
        for base in routes {
            let selected = self.selected == Some(base.key);
            let changed = self.routes.contains_key(&base.key);
            if !selected && !changed && hover != Some(base.key) {
                continue;
            }
            let route = self.routes.get(&base.key).unwrap_or(base);
            let drag = self.drag.filter(|d| d.route == route.key && d.next != d.origin);
            let color = if changed || drag.is_some() {
                Color32::from_rgb(255, 213, 138)
            } else {
                Color32::from_rgb(142, 201, 255)
            };
            if changed || drag.is_some() {
                let mut spans = self.changed_segments.entry(route.key)
                    .or_insert_with(|| changed_segments(&base.controls, &route.controls)).clone();
                if let Some(drag) = drag {
                    let (first, last) = match drag.change.operation {
                        Operation::Bend { first, last, .. } => (first + 1, last - 1),
                        _ => (drag.control, drag.control),
                    };
                    // Adjacent tangents influence an extra segment on either side.
                    let end = route.controls.len() - 1;
                    spans.push(first.saturating_sub(2)..(last + 2).min(end));
                    if route.controls[0] == route.controls[end] {
                        if first == 1 {
                            spans.push(end - 1..end);
                        }
                        if last == end - 1 {
                            spans.push(0..1);
                        }
                    }
                    spans.sort_unstable_by_key(|span| span.start);
                }
                if !spans.is_empty() {
                    let cached = (drag.is_none() && fits_curve_cache(route))
                        .then(|| self.curves.curve(route));
                    let curve = cached.is_none().then(|| control_path(route, drag)).flatten();
                    let mut spans = spans.into_iter().peekable();
                    while let Some(mut span) = spans.next() {
                        while let Some(next) = spans.next_if(|next| next.start <= span.end) {
                            span.end = span.end.max(next.end);
                        }
                        let points = if let Some(cached) = cached {
                            let subdivisions = (cached.points.len() - 1) / (route.controls.len() - 1);
                            cached.points[span.start * subdivisions..=span.end * subdivisions]
                                .iter().copied().map(|p| screen(map, size, p)).collect::<Vec<_>>()
                        } else if let Some(curve) = &curve {
                            sample_span(curve, span)
                                .map(|p| screen(map, size, p))
                                .collect()
                        } else {
                            (span.start..=span.end)
                                .map(|i| screen(map, size, preview_point(route, i, drag)))
                                .collect()
                        };
                        painter.extend(egui::Shape::dashed_line(
                            &points, Stroke::new(2., color), 7., 4.,
                        ));
                    }
                }
            } else {
                let line = self.curves.line(route, None, map, size);
                painter.add(egui::Shape::line(line, Stroke::new(2., color)));
            }
            if selected && editable {
                for i in visible_controls(route, map, size, self.control) {
                    let point = preview_point(route, i, drag);
                    let point = screen(map, size, point);
                    if !painter.clip_rect().expand(7.).contains(point) {
                        continue;
                    }
                    let radius = if self.control == Some(i) { 5. } else { 3. };
                    painter.circle(
                        point,
                        radius,
                        if route.is_fixed(i) {
                            Color32::from_gray(38)
                        } else {
                            color
                        },
                        Stroke::new(
                            1.,
                            if route.is_fixed(i) {
                                color
                            } else {
                                Color32::from_gray(38)
                            },
                        ),
                    );
                }
            }
        }
    }
}

fn visible_controls(
    route: &Route, map: Rect, size: u32, selected: Option<usize>,
) -> impl Iterator<Item = usize> + '_ {
    let world_spacing = CONTROL_SPACING * size as f32 / map.width();
    let mut previous = route.controls[0];
    let mut last_handle = 0;
    let mut distance = 0.;
    route.controls.iter().enumerate().filter_map(move |(i, &point)| {
        distance += (Vec2::from(point) - Vec2::from(previous)).length();
        previous = point;
        // Keep a bend handle even on short routes, and never hide an anchor.
        if Some(i) == selected || route.is_fixed(i) || i == route.controls.len() / 2
            || (i > last_handle && distance >= world_spacing)
        {
            last_handle = i;
            distance = 0.;
            Some(i)
        } else {
            None
        }
    })
}

fn bend_control(route: &Route, segment: usize, map: Rect, size: u32, pointer: Pos2) -> Option<usize> {
    [segment, segment + 1].into_iter()
        .filter(|&i| !route.is_fixed(i))
        .map(|i| (i, screen(map, size, route.controls[i]).distance_sq(pointer)))
        .filter(|&(_, distance)| distance <= DIRECT_BEND_RADIUS.powi(2))
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

fn preview_point(route: &Route, index: usize, drag: Option<Drag>) -> [f32; 2] {
    let point = route.controls[index];
    let Some(drag) = drag else { return point; };
    if index == drag.control {
        return drag.next;
    }
    let weight = drag.change.bend_weight(index);
    std::array::from_fn(|axis| point[axis] + (drag.next[axis] - drag.origin[axis]) * weight)
}

fn preview_controls(route: &Route, drag: Option<Drag>) -> impl Iterator<Item = [f32; 2]> + '_ {
    (0..route.controls.len()).map(move |i| preview_point(route, i, drag))
}

fn control_path(route: &Route, drag: Option<Drag>) -> Option<papyrus_terrain::path_geometry::Path> {
    let points: Vec<_> = preview_controls(route, drag)
        .map(|[x, z]| [x, 0., z])
        .collect();
    papyrus_terrain::path_geometry::Path::from_controls(&points, 1_000_000, || false)
        .ok()
        .flatten()
}

fn sample_span(
    path: &papyrus_terrain::path_geometry::Path, span: Range<usize>,
) -> impl Iterator<Item = [f32; 2]> + '_ {
    let sample_count = (path.points().len() - 1) * CURVE_SUBDIVISIONS;
    (span.start * CURVE_SUBDIVISIONS..=span.end * CURVE_SUBDIVISIONS).filter_map(move |i| {
        path.point(path.length() * (i as f32 / sample_count as f32))
            .ok()
            .map(|[x, _, z]| [x, z])
    })
}

fn sample_curve(route: &Route, drag: Option<Drag>) -> Vec<[f32; 2]> {
    match control_path(route, drag) {
        Some(path) => sample_span(&path, 0..route.controls.len() - 1).collect(),
        None => preview_controls(route, drag).collect(),
    }
}

fn project_segment(map: Rect, size: u32, pointer: Pos2, segment: &[[f32; 2]]) -> (f32, Pos2) {
    let a = screen(map, size, segment[0]);
    let b = screen(map, size, segment[1]);
    let v = b - a;
    let t = ((pointer - a).dot(v) / v.length_sq().max(0.0001)).clamp(0., 1.);
    let projection = a + t * v;
    (pointer.distance_sq(projection), projection)
}

fn screen(map: Rect, size: u32, [x, z]: [f32; 2]) -> Pos2 {
    let size = size as f32;
    egui::pos2(
        map.left() + (x / size + 0.5) * map.width(),
        map.top() + (0.5 - z / size) * map.height(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use mapper_studio::path_edit::Kind;

    #[test]
    fn restored_draft_preserves_order_and_rejects_invalid_or_excess_edits() {
        let route = Route {
            key: Key { kind: Kind::Road, index: 0 },
            controls: vec![[-200., 0.], [0., 0.], [200., 0.]],
            fixed: vec![0, 2],
        };
        let routes = std::slice::from_ref(&route);
        let move_point = |expected, next| Change {
            route: route.key,
            control: 1,
            expected: [0., expected],
            operation: Operation::Move([0., next]),
        };
        let changes: Vec<_> = (0..4095).map(|i| {
            move_point((i % 2) as f32, (1 - i % 2) as f32)
        }).collect();
        let mut invalid = changes.clone();
        invalid.push(move_point(0., 50.));
        assert!(Draft::restore(routes, invalid, 1000).is_err());
        let mut draft = Draft::restore(routes, changes.clone(), 1000).unwrap();
        assert_eq!(draft.changes(), changes);
        assert_eq!(draft.displayed(&route).controls[1], [0., 1.]);
        assert!(draft.record(routes, move_point(1., f32::NAN), 1000).is_err());
        assert_eq!(draft.changes(), changes);
        let final_move = move_point(1., 2.);
        draft.record(routes, final_move, 1000).unwrap();
        assert!(draft.record(routes, move_point(2., 3.), 1000).is_err());
        assert_eq!(draft.changes().last(), Some(&final_move));
        assert_eq!(draft.displayed(&route).controls[1], [0., 2.]);
    }

    #[test]
    fn curved_centerline_picks_the_visible_segment_before_selection() {
        let size = 1000;
        let map = Rect::from_min_size(Pos2::ZERO, Vec2::splat(1000.));
        let route = Route {
            key: Key { kind: Kind::Rail, index: 0 },
            controls: vec![[-400., 0.], [-200., 0.], [-200., 300.], [200., 300.]],
            fixed: vec![0, 3],
        };
        let mut draft = Draft::default();
        let points = sample_curve(&route, None);
        let pointer = screen(map, size, points[2]);
        assert!(route.controls.windows(2).all(|segment| {
            project_segment(map, size, pointer, segment).0 > PICK_RADIUS * PICK_RADIUS
        }));
        let (distance, key, segment, projected) = draft
            .nearest(std::slice::from_ref(&route), map, size, pointer)
            .unwrap();
        assert!(distance < 0.001);
        assert_eq!(key, route.key);
        assert_eq!(segment, 0);
        assert_eq!(projected, pointer);
    }

    #[test]
    fn spline_history_restores_insertions_removals_and_fixed_indices() {
        let base = Route {
            key: Key {
                kind: Kind::Trail,
                index: 0,
            },
            controls: vec![[0., 0.], [10., 0.], [20., 0.]],
            fixed: vec![0, 2],
        };
        let routes = std::slice::from_ref(&base);
        let insert = Change {
            route: base.key,
            control: 0,
            expected: [0., 0.],
            operation: Operation::Insert([5., 1.]),
        };
        let remove = Change {
            route: base.key,
            control: 2,
            expected: [10., 0.],
            operation: Operation::Remove,
        };
        let mut draft = Draft::default();
        draft.record(routes, insert, 1000).unwrap();
        draft.record(routes, remove, 1000).unwrap();
        assert_eq!(draft.displayed(&base).fixed, [0, 2]);
        draft.step_history(routes, remove, 1000, false).unwrap();
        assert_eq!(draft.displayed(&base).fixed, [0, 3]);
        assert_eq!(draft.displayed(&base).controls[2], [10., 0.]);
        draft.step_history(routes, insert, 1000, false).unwrap();
        assert_eq!(draft.displayed(&base).controls, base.controls);
        draft.step_history(routes, insert, 1000, true).unwrap();
        draft.step_history(routes, remove, 1000, true).unwrap();
        assert_eq!(
            draft.displayed(&base).controls,
            [[0., 0.], [5., 1.], [20., 0.]]
        );
        assert_eq!(draft.displayed(&base).fixed, [0, 2]);
    }

    #[test]
    fn dense_routes_offer_fewer_handles_and_preview_matches_bend_replay() {
        let route = Route {
            key: Key { kind: Kind::Rail, index: 0 },
            controls: (0..201).map(|i| [i as f32 * 2. - 200., 0.]).collect(),
            fixed: vec![0, 96, 200],
        };
        let map = Rect::from_min_size(Pos2::ZERO, Vec2::splat(1000.));
        let handles: Vec<_> = visible_controls(&route, map, 1000, None).collect();
        let closer = visible_controls(&route, map.scale_from_center(2.), 1000, None).count();
        assert!(handles.len() < 25 && closer > handles.len());
        assert!([0, 96, 100, 200].iter().all(|i| handles.contains(i)));
        let hidden = 112;
        assert!(!handles.contains(&hidden));
        let pointer = screen(map, 1000, route.controls[hidden]);
        assert_eq!(bend_control(&route, hidden - 1, map, 1000, pointer), Some(hidden));

        let control = 100;
        let origin = route.controls[control];
        let next = [0., 12.];
        let drag = Drag {
            route: route.key, control, press: Pos2::ZERO, origin, next, moved: true,
            fixed: false, on_curve: false, change: route.drag_change(control, next),
        };
        let change = drag.change;
        assert!(matches!(change.operation, Operation::Bend { first: 96, last: 132, .. }));
        let draft = Draft::restore(std::slice::from_ref(&route), vec![change], 1000).unwrap();
        let points = &draft.displayed(&route).controls;
        assert_eq!(*points, preview_controls(&route, Some(drag)).collect::<Vec<_>>());
        let path = control_path(&route, Some(drag)).unwrap();
        let full = sample_curve(&route, Some(drag));
        assert_eq!(sample_span(&path, 95..134).collect::<Vec<_>>(),
            full[95 * CURVE_SUBDIVISIONS..=134 * CURVE_SUBDIVISIONS]);
        assert_eq!(points[96], route.controls[96]);
        assert_eq!(points[132], route.controls[132]);
        assert!(points[110][1] > points[120][1] && points[120][1] > 0.);
    }

    #[test]
    fn draft_highlights_only_the_edited_segment_neighbourhoods() {
        let base = Route {
            key: Key { kind: Kind::Rail, index: 0 },
            controls: (0..30).map(|x| [x as f32 * 10., 0.]).collect(),
            fixed: vec![0, 29],
        };
        let mut draft = base.clone();
        draft.apply(Change {
            route: base.key, control: 4, expected: [40., 0.],
            operation: Operation::Insert([45., 3.]),
        }).unwrap();
        draft.apply(Change {
            route: base.key, control: 21, expected: [200., 0.],
            operation: Operation::Move([200., 10.]),
        }).unwrap();
        assert_eq!(changed_segments(&base.controls, &draft.controls), vec![3..7, 19..23]);
        assert!(changed_segments(&base.controls, &base.controls).is_empty());
    }

    #[test]
    fn indexed_picking_matches_full_scans_and_refreshes_changed_curves() {
        let size = 4000;
        let mut routes: Vec<_> = (0..32)
            .map(|index| Route {
                key: Key {
                    kind: Kind::Road,
                    index,
                },
                controls: (0..513)
                    .map(|i| [i as f32 * 5. - 1280., index as f32 * 100. - 1550.])
                    .collect(),
                fixed: vec![0, 512],
            })
            .collect();
        // Overlapping segments keep their source-order tie break.
        routes[1].controls = routes[0].controls.clone();
        let mut draft = Draft::default();
        let map = Rect::from_min_size(Pos2::new(-131., -22.), Vec2::splat(2048.));
        let pans = [Vec2::ZERO, egui::vec2(-380., 163.)];
        let mut probes = Vec::new();
        for zoom in [0.25, 1., 8.] {
            for pan in pans {
                let viewport = Rect::from_center_size(map.center() + pan, map.size() * zoom);
                for [x, z] in [[-1120., -1550.], [0., 750.], [1750., -1550.], [5., 1520.]] {
                    let pointer = screen(viewport, size, [x, z]);
                    for offset in [-8., -7.75, 0., 8., 8.25] {
                        probes.push((viewport, pointer + egui::vec2(0., offset)));
                    }
                }
            }
        }
        let curves: Vec<_> = routes.iter().map(|route| sample_curve(route, None)).collect();
        let start = std::time::Instant::now();
        let expected: Vec<_> = probes
            .iter()
            .map(|&(map, pointer)| {
                let mut nearest: Option<(f32, Key, usize, Pos2)> = None;
                for (route, points) in routes.iter().zip(&curves) {
                    for (i, segment) in points.windows(2).enumerate() {
                        let (distance, point) = project_segment(map, size, pointer, segment);
                        if distance <= PICK_RADIUS * PICK_RADIUS
                            && nearest.as_ref().is_none_or(|&(d, ..)| distance < d)
                        {
                            nearest = Some((distance, route.key, i / CURVE_SUBDIVISIONS, point));
                        }
                    }
                }
                nearest
            })
            .collect();
        let scan = start.elapsed();
        let _ = draft.nearest(&routes, map, size, map.center());
        let start = std::time::Instant::now();
        let actual: Vec<_> = probes
            .iter()
            .map(|&(map, pointer)| draft.nearest(&routes, map, size, pointer))
            .collect();
        let indexed = start.elapsed();
        assert_eq!(actual, expected);
        eprintln!(
            "Spline pick replay, {} probes over {} segments: {scan:?} scan, {indexed:?} indexed",
            probes.len(),
            32 * 512
        );

        let base = &routes[0];
        let original_line = draft.curves.line(base, None, map, size);
        let new = [0., -1800.];
        let pointer = screen(map, size, new);
        assert!(draft.nearest(&routes, map, size, pointer).is_none());
        let drag = Drag {
            route: base.key,
            control: 256,
            press: pointer,
            origin: base.controls[256],
            next: new,
            moved: true,
            fixed: false,
            on_curve: false,
            change: Change {
                route: base.key, control: 256, expected: base.controls[256],
                operation: Operation::Move(new),
            },
        };
        let preview = draft.curves.line(base, Some(drag), map, size);
        assert_ne!(preview, original_line);
        assert_eq!(draft.curves.line(base, None, map, size), original_line);
        draft
            .record(
                &routes,
                Change {
                    route: base.key,
                    control: drag.control,
                    expected: drag.origin,
                    operation: Operation::Move(new),
                },
                size,
            )
            .unwrap();
        assert_eq!(
            draft.nearest(&routes, map, size, pointer).unwrap().1,
            base.key
        );
        assert_eq!(
            draft.curves.line(&draft.routes[&base.key], None, map, size),
            preview
        );
    }
}
