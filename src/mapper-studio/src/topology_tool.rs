use super::*;
use papyrus_terrain::topology_paint::{
    BRUSH_VERSION, Edits, Operation, RESTORE_VERSION, Shape, Stroke as BrushStroke, VERSION,
};
#[derive(Default)]
pub(super) struct Tool {
    pub enabled: bool,
    radius: f32,
    operation: Option<Operation>,
    shape: Shape,
    layers: Option<Vec<String>>,
    mask: Option<u32>,
    overlay_changed: bool,
    group_name: String,
    draft_source: Option<(String, u32)>,
    draft: Option<(Project, BrushStroke)>,
    last: Option<(Project, egui::TextureHandle)>,
    pub message: String,
}
fn same_base(a: &Project, b: &Project) -> bool {
    if a == b {
        return true;
    }
    let mut a = a.clone();
    let mut b = b.clone();
    a.topology_edits = None;
    b.topology_edits = None;
    a == b
}
fn operation_label(operation: Operation) -> &'static str {
    match operation {
        Operation::Add => "Add",
        Operation::Remove => "Erase",
        Operation::Restore => "Restore generated",
        Operation::Invert => "Invert",
    }
}
fn square_sweep(a: Pos2, b: Pos2, radius: f32) -> Vec<Pos2> {
    let mut corners = Vec::with_capacity(8);
    for p in [a, b] {
        for x in [-radius, radius] {
            for y in [-radius, radius] {
                corners.push(p + egui::vec2(x, y));
            }
        }
    }
    corners.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    corners.dedup();
    let cross = |a: Pos2, b: Pos2, c: Pos2| (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
    let mut lower: Vec<Pos2> = vec![];
    let mut upper: Vec<Pos2> = vec![];
    for &p in &corners {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], p) <= 0. {
            lower.pop();
        }
        lower.push(p);
    }
    for &p in corners.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], p) <= 0. {
            upper.pop();
        }
        upper.push(p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}
impl Tool {
    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.enabled = enabled;
            self.cancel_stroke();
            self.overlay_changed = true;
        }
    }

    pub fn selection_mask(&self, bit: u8) -> u32 {
        self.mask.unwrap_or(1_u32 << bit)
    }
    pub fn take_overlay_change(&mut self) -> bool {
        std::mem::take(&mut self.overlay_changed)
    }
    pub fn cancel_stroke(&mut self) {
        self.draft = None;
        self.draft_source = None;
        self.message.clear();
    }
    pub fn remember(&mut self, project: &Project, texture: &egui::TextureHandle) {
        if self
            .last
            .as_ref()
            .is_none_or(|(p, t)| p != project || t.id() != texture.id())
        {
            self.last = Some((project.clone(), texture.clone()));
        }
    }
    pub fn pending_texture(&self, project: &Project) -> Option<&egui::TextureHandle> {
        self.last
            .as_ref()
            .filter(|(p, _)| same_base(p, project))
            .map(|(_, t)| t)
    }
    #[cfg(test)]
    pub fn controls(&mut self, ui: &mut egui::Ui, project: &mut Project, ready: bool, bit: u8) {
        self.controls_with_layers(
            ui,
            project,
            ready,
            bit,
            &[("Test flag".into(), 1_u32 << bit)],
            &mut vec![],
        );
    }
    pub fn controls_with_layers(
        &mut self,
        ui: &mut egui::Ui,
        project: &mut Project,
        ready: bool,
        bit: u8,
        flags: &[(String, u32)],
        groups: &mut Vec<mapper_studio::topology_presets::Preset>,
    ) -> bool {
        let mut groups_changed = false;
        if !mapper_studio::native_preview::is_native(&project.preview_engine)
            || project.composition_engine.is_none()
        {
            self.enabled = false;
            self.draft = None;
            self.last = None;
        }
        if self.radius == 0. {
            self.radius = 25.;
        }
        ui.horizontal_wrapped(|ui| {
            if self.enabled {
                ui.add_enabled_ui(self.draft.is_none(), |ui| {
                    let mut mode = self.operation.unwrap_or(Operation::Add);
                    egui::ComboBox::from_id_salt("topology-operation")
                        .selected_text(operation_label(mode)).show_ui(ui, |ui| {
                            for op in [Operation::Add, Operation::Remove, Operation::Invert, Operation::Restore] {
                                let icon = match op {
                                    Operation::Add => Some(super::tool_icons::Icon::Brush),
                                    Operation::Remove => Some(super::tool_icons::Icon::Eraser),
                                    _ => None,
                                };
                                let label = operation_label(op);
                                if let Some(icon) = icon {
                                    if ui.add(egui::Button::image_and_text(icon.image(ui), label)
                                        .selected(mode == op)).clicked()
                                    {
                                        mode = op;
                                    }
                                } else {
                                    ui.selectable_value(&mut mode, op, label);
                                }
                            }
                        }).response.on_hover_text("Add sets selected flags; Erase clears them; Invert flips them once per stroke; Restore uses the generated baseline. Unselected flags are preserved.");
                    self.operation = Some(mode);
                    egui::ComboBox::from_id_salt("topology-brush-shape")
                        .selected_text(match self.shape { Shape::Round => "Round", Shape::Square => "Square" })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.shape, Shape::Round, "Round");
                            ui.selectable_value(&mut self.shape, Shape::Square, "Square");
                        });
                    ui.label("Radius (m)");
                    ui.add(
                        egui::DragValue::new(&mut self.radius)
                            .range(0.5..=256.)
                            .speed(0.5),
                    );
                });
            }
        });
        if self.enabled {
            ui.add_enabled_ui(self.draft.is_none(), |ui| {
                ui.horizontal_wrapped(|ui| {
                    let selected = self.selected_names(flags, bit);
                    egui::containers::menu::MenuButton::new(format!("Layers ({})", selected.len()))
                        .config(egui::containers::menu::MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside))
                        .ui(ui, |ui| {
                        if ui.button("Follow inspection layer").clicked() { self.layers=None; }
                        ui.horizontal(|ui| {
                            if ui.button("All").clicked() { self.layers=Some(flags.iter().map(|(n,_)|n.clone()).collect()); }
                            if ui.button("None").clicked() { self.layers=Some(vec![]); }
                        });
                        ui.separator();
                        egui::ScrollArea::vertical().max_height(250.).show(ui, |ui| {
                            for (name, _) in flags {
                                let mut checked = self.selected_names(flags, bit).contains(name);
                                if ui.checkbox(&mut checked, name).changed() {
                                    let mut names=self.selected_names(flags, bit);
                                    names.retain(|n|n!=name);
                                    if checked { names.push(name.clone()); }
                                    self.layers=Some(names);
                                }
                            }
                            if flags.is_empty() { ui.label("Installation layers unavailable"); }
                        });
                    }).0.on_hover_text("One stroke changes all selected layers together. Inspection can still show an individual flag.");
                    egui::containers::menu::MenuButton::new("Groups")
                        .config(egui::containers::menu::MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside))
                        .ui(ui, |ui| {
                        let mut remove=None;
                        for (index, group) in groups.iter().enumerate() {
                            let resolved=mapper_studio::topology_presets::resolve(&group.layers, flags);
                            ui.horizontal(|ui| {
                                let response=ui.add_enabled(resolved.is_ok(), egui::Button::new(&group.name));
                                let response=if let Err(reason)=&resolved { response.on_disabled_hover_text(reason) }else{response};
                                if response.clicked() {self.layers=Some(group.layers.clone());ui.close();}
                                if ui.small_button("Delete").on_hover_text("Delete this saved group; map strokes are unchanged.").clicked() {remove=Some(index);}
                            });
                        }
                        if let Some(index)=remove { groups.remove(index); groups_changed=true; }
                        if groups.is_empty() {ui.label("No saved groups");}
                        ui.separator();
                        ui.add(egui::TextEdit::singleline(&mut self.group_name).hint_text("Group name").char_limit(64).desired_width(180.));
                        let names=self.selected_names(flags, bit);
                        let update=groups.iter().position(|p|p.name.eq_ignore_ascii_case(self.group_name.trim()));
                        let allowed=!self.group_name.trim().is_empty() && mapper_studio::topology_presets::resolve(&names,flags).is_ok()
                            && (update.is_some() || groups.len()<32);
                        if ui.add_enabled(allowed, egui::Button::new(if update.is_some(){"Update group"}else{"Save selected layers"})).clicked() {
                            let mut next=groups.clone();
                            let group=mapper_studio::topology_presets::Preset{name:self.group_name.trim().into(),layers:names};
                            if let Some(index)=update {next[index]=group;}else{next.push(group);}
                            match mapper_studio::topology_presets::validate(&next) {
                                Ok(())=>{*groups=next;groups_changed=true;self.group_name.clear();ui.close();}
                                Err(reason)=>self.message=reason,
                            }
                        }
                    }).0.on_hover_text("Named layer selections saved for every map. Layers are matched by installation names, not fixed bit numbers.");
                });
            });
        }
        let resolved =
            mapper_studio::topology_presets::resolve(&self.selected_names(flags, bit), flags);
        let selection_error = resolved.as_ref().err();
        let next_mask = Some(resolved.as_ref().copied().unwrap_or(0));
        self.overlay_changed |= next_mask != self.mask;
        self.mask = next_mask;
        if self.enabled || project.topology_edits.is_some() {
            ui.horizontal_wrapped(|ui| {
                ui.add_enabled_ui(self.draft.is_none() && project.topology_edits.is_some(), |ui| {
                    ui.menu_button("Painted edits", |ui| {
                        if let Some(edits) = &mut project.topology_edits {
                            let mut enabled = edits.enabled;
                            if ui.checkbox(&mut enabled, "Apply painted edits").changed() {
                                Arc::make_mut(edits).enabled = enabled;
                            }
                            ui.small(format!("{} strokes", edits.strokes.len()));
                        }
                        let mask = self.selection_mask(bit);
                        let has_edits = project.topology_edits.as_ref().is_some_and(|edits| {
                            edits.strokes.iter().any(|stroke| stroke.mask & mask != 0)
                        });
                        if ui.add_enabled(has_edits, egui::Button::new("Reset selected layers"))
                            .on_hover_text("Restore the generated values across the map. Undo restores the edits.")
                            .clicked()
                        {
                            self.reset_selected(project, mask);
                        }
                        if ui.button("Clear painted edits").clicked() {
                            project.topology_edits = None;
                        }
                    });
                });
                if self.enabled {
                    let (message, hint) = if !ready {
                        ("Waiting for composed topology…", "Painting becomes available when composition finishes.")
                    } else if let Some(reason) = selection_error {
                        (reason.as_str(), reason.as_str())
                    } else if self.message.is_empty() {
                        ("Drag to paint", "Escape cancels the current stroke. Undo restores the previous layer.")
                    } else {
                        (self.message.as_str(), self.message.as_str())
                    };
                    ui.add(egui::Label::new(egui::RichText::new(message).small()).truncate())
                        .on_hover_text(hint);
                }
            });
        }
        groups_changed
    }
    fn selected_names(&self, flags: &[(String, u32)], bit: u8) -> Vec<String> {
        self.layers.clone().unwrap_or_else(|| {
            flags
                .iter()
                .filter(|(_, mask)| *mask == 1_u32 << bit)
                .map(|(n, _)| n.clone())
                .collect()
        })
    }
    fn reset_selected(&mut self, project: &mut Project, mask: u32) {
        let Some(edits) = &mut project.topology_edits else { return };
        if !edits.strokes.iter().any(|stroke| stroke.mask & mask != 0) {
            return;
        }
        let edits = Arc::make_mut(edits);
        edits.reset_mask(mask);
        if edits.strokes.is_empty() {
            project.topology_edits = None;
        }
        self.message =
            "Selected layers restored to the generated map everywhere. Undo restores their edits."
                .into();
    }
    pub fn paint(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        map: Rect,
        project: &mut Project,
        bit: u8,
        source: Option<(String, u32)>,
    ) {
        if !self.enabled
            || source.is_none()
            || self.draft_source.as_ref().is_some_and(|s| Some(s) != source.as_ref())
            || ui.input(|i| {
                !i.focused
                    || i.raw.events.iter().any(|event| matches!(event, egui::Event::PointerGone))
                    || i.key_pressed(egui::Key::Escape)
                || i.modifiers.command
                    && (i.key_pressed(egui::Key::Z) || i.key_pressed(egui::Key::Y))
            })
            || self.draft.as_ref().is_some_and(|(p, _)| p != project)
        {
            self.cancel_stroke();
            return;
        }
        let size = project.size as f32;
        let world = |p: Pos2| {
            [
                (((p.x - map.left()) / map.width() - 0.5) * size).clamp(-size / 2., size / 2.),
                ((0.5 - (p.y - map.top()) / map.height()) * size).clamp(-size / 2., size / 2.),
            ]
        };
        let screen = |p: [f32; 2]| {
            egui::pos2(
                map.left() + (p[0] / size + 0.5) * map.width(),
                map.top() + (0.5 - p[1] / size) * map.height(),
            )
        };
        let press = ui.input(|i| i.raw.events.iter().find_map(|event| match *event {
            egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, .. } => Some(pos),
            _ => None,
        }));
        let starting = self.draft.is_none();
        if response.enabled()
            && starting
            && let Some(pos) = press
                .filter(|p| map.contains(*p) && response.interact_rect.contains(*p))
                .filter(|p| ui.ctx().layer_id_at(*p).is_none_or(|layer| layer == response.layer_id))
        {
            let mask = self.mask.unwrap_or(1_u32 << bit);
            if mask != 0 && source.as_ref().is_some_and(|(_, known)| mask & !known == 0) {
                self.draft_source = source.clone();
                self.draft = Some((
                    project.clone(),
                    BrushStroke {
                        shape: self.shape,
                        operation: self.operation.unwrap_or(Operation::Add),
                        mask,
                        radius: self.radius,
                        points: vec![world(pos)],
                    },
                ));
                self.message.clear();
            } else {
                self.message =
                    "Choose layers available in this installation before painting.".into();
            }
        }
        let mut released = false;
        if let Some((_, stroke)) = &mut self.draft {
            let reached_limit = ui.input(|i| {
                let mut sampling = !starting;
                let mut reached_limit = false;
                for event in &i.raw.events {
                    let pos = match *event {
                        egui::Event::PointerMoved(pos) if sampling => pos,
                        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, .. } => {
                            if sampling == pressed { continue; }
                            sampling = pressed;
                            released = !pressed;
                            pos
                        }
                        _ => continue,
                    };
                    let point = world(pos);
                    if stroke.points.last().is_some_and(|p| (p[0] - point[0]).hypot(p[1] - point[1]) >= 0.1) {
                        if stroke.points.len() == 4096 {
                            reached_limit = true;
                        } else {
                            stroke.points.push(point);
                        }
                    }
                    if released {
                        break;
                    }
                }
                reached_limit
            });
            if reached_limit {
                self.message = "Stroke point limit reached. Release to finish.".into();
            }
            let color = match stroke.operation {
                Operation::Add => Color32::from_rgba_unmultiplied(255, 255, 255, 210),
                Operation::Remove => Color32::from_rgba_unmultiplied(20, 20, 20, 230),
                Operation::Restore => Color32::from_rgba_unmultiplied(70, 175, 220, 150),
                Operation::Invert => Color32::from_rgba_unmultiplied(235, 170, 60, 180),
            };
            let radius = stroke.radius / size * map.width();
            let painter = ui.painter().with_clip_rect(map.intersect(response.rect));
            for pair in stroke.points.windows(2) {
                if stroke.shape == Shape::Square {
                    painter.add(egui::Shape::convex_polygon(
                        square_sweep(screen(pair[0]), screen(pair[1]), radius),
                        color,
                        Stroke::NONE,
                    ));
                } else {
                    painter.line_segment(
                        [screen(pair[0]), screen(pair[1])],
                        Stroke::new(radius * 2., color),
                    );
                }
            }
            for p in &stroke.points {
                if stroke.shape == Shape::Square {
                    painter.rect_filled(
                        Rect::from_center_size(screen(*p), egui::Vec2::splat(radius * 2.)),
                        0.,
                        color,
                    );
                } else {
                    painter.circle_filled(screen(*p), radius, color);
                }
            }
        }
        if let Some(pos) = response.hover_pos().filter(|p| map.contains(*p)) {
            let radius = self.radius / size * map.width();
            let outline = Stroke::new(1.5, Color32::from_rgb(255, 205, 92));
            if self.shape == Shape::Square {
                ui.painter().rect_stroke(
                    Rect::from_center_size(pos, egui::Vec2::splat(radius * 2.)),
                    0.,
                    outline,
                    egui::StrokeKind::Inside,
                );
            } else {
                ui.painter().circle_stroke(pos, radius, outline);
            }
        }
        if released
            && let Some((_, stroke)) = self.draft.take()
            && let Some((sha, _)) = source
        {
            self.draft_source = None;
            let mut next = project.clone();
            let edits = next.topology_edits.get_or_insert_with(|| Arc::new(Edits {
                engine: VERSION.into(),
                source_sha256: sha.clone(),
                size: project.size,
                enabled: true,
                strokes: vec![],
            }));
            if edits.source_sha256 != sha {
                self.message="Manual layer belongs to different installation metadata; clear it before painting with this source.".into();
                return;
            }
            let edits = Arc::make_mut(edits);
            edits.enabled = true;
            if stroke.shape != Shape::Round || stroke.operation == Operation::Invert {
                edits.engine = BRUSH_VERSION.into();
            } else if stroke.operation == Operation::Restore && edits.engine == VERSION {
                edits.engine = RESTORE_VERSION.into();
            }
            edits.strokes.push(stroke);
            match next.validate() {
                Ok(()) => {
                    *project = next;
                    self.message.clear();
                }
                Err(e) => self.message = e,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn project() -> Project {
        Project {
            preview_engine: mapper_studio::native_preview::VERSION.into(),
            installation: Some(mapper_studio::installation::Installation {
                kind: mapper_studio::installation::InstallationKind::Client,
                native: mapper_studio::installation::SourceFile {
                    relative_path: "RustClient_Data/Plugins/x86_64/RustNative.dll".into(),
                    sha256: "a".repeat(64),
                    bytes: 1,
                },
                scene_manifest: None,
            }),
            ..Default::default()
        }
    }
    fn frame(ctx: &egui::Context, tool: &mut Tool, p: &mut Project, events: Vec<egui::Event>) {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(600., 600.))),
                events,
                focused: true,
                ..Default::default()
            },
            |ui| {
                let map = Rect::from_min_size(egui::pos2(50., 50.), egui::vec2(500., 500.));
                let response =
                    ui.interact(map, egui::Id::new("paint-test"), Sense::click_and_drag());
                tool.paint(ui, &response, map, p, 1, Some(("b".repeat(64), 3)));
            },
        );
        output.textures_delta.clear();
    }
    fn pointer(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        }
    }
    #[test]
    fn dragging_creates_one_saved_stroke_without_touching_monuments_or_seeds() {
        let ctx = egui::Context::default();
        let mut t = Tool {
            enabled: true,
            radius: 25.,
            ..Default::default()
        };
        let mut p = project();
        let before = p.clone();
        frame(&ctx, &mut t, &mut p, vec![]);
        let a = egui::pos2(100., 100.);
        let b = egui::pos2(250., 250.);
        frame(
            &ctx,
            &mut t,
            &mut p,
            vec![egui::Event::PointerMoved(a), pointer(a, true)],
        );
        assert_eq!(p, before);
        frame(&ctx, &mut t, &mut p, vec![egui::Event::PointerMoved(b)]);
        assert_eq!(p, before);
        frame(&ctx, &mut t, &mut p, vec![pointer(b, false)]);
        assert_eq!(p.targets, before.targets);
        assert_eq!(p.seeds, before.seeds);
        assert_eq!(p.terrain_key(), before.terrain_key());
        let edits = p.topology_edits.unwrap();
        assert_eq!(edits.strokes.len(), 1);
        assert_eq!(edits.strokes[0].points.len(), 2);
    }
    #[test]
    fn short_stroke_keeps_its_bend_and_pointer_loss_cancels_the_next_stroke() {
        let ctx = egui::Context::default();
        let mut tool = Tool { enabled: true, radius: 25., ..Default::default() };
        let mut project = project();
        let a = egui::pos2(100., 100.);
        let b = egui::pos2(250., 100.);
        let c = egui::pos2(250., 250.);
        frame(&ctx, &mut tool, &mut project, vec![]);
        frame(&ctx, &mut tool, &mut project, vec![
            pointer(a, true),
            egui::Event::PointerMoved(b),
            pointer(c, false),
        ]);
        let edits = project.topology_edits.as_ref().unwrap();
        assert_eq!(edits.strokes.len(), 1);
        let points = &edits.strokes[0].points;
        assert_eq!(points.len(), 3);
        assert_eq!(points[0][1], points[1][1]);
        assert_eq!(points[1][0], points[2][0]);
        let saved = project.clone();
        frame(&ctx, &mut tool, &mut project, vec![pointer(a, true)]);
        assert!(tool.draft.is_some());
        frame(&ctx, &mut tool, &mut project, vec![egui::Event::PointerGone]);
        frame(&ctx, &mut tool, &mut project, vec![pointer(c, false)]);
        assert_eq!(project, saved);
    }
    #[test]
    fn clicking_a_floating_panel_does_not_paint_the_map_underneath() {
        let ctx = egui::Context::default();
        let mut tool = Tool { enabled: true, radius: 25., ..Default::default() };
        let mut project = project();
        let before = project.clone();
        let pos = egui::pos2(200., 200.);
        let panel = egui::Id::new("floating-tools-test");
        for events in [vec![], vec![], vec![pointer(pos, true)], vec![pointer(pos, false)]] {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(600., 600.))),
                    events,
                    focused: true,
                    ..Default::default()
                },
                |ui| {
                    egui::Area::new(panel)
                        .order(egui::Order::Foreground)
                        .fixed_pos(egui::pos2(150., 150.))
                        .show(ui.ctx(), |ui| ui.set_min_size(egui::vec2(125., 125.)));
                    let map = Rect::from_min_size(Pos2::ZERO, egui::vec2(500., 500.));
                    let response = ui.interact(map, egui::Id::new("paint-test"), Sense::click_and_drag());
                    tool.paint(ui, &response, map, &mut project, 1, Some(("b".repeat(64), 3)));
                },
            );
            output.textures_delta.clear();
        }
        assert_eq!(ctx.layer_id_at(pos), Some(egui::LayerId::new(egui::Order::Foreground, panel)));
        assert_eq!(project, before);
        assert!(tool.draft.is_none());
    }
    #[test]
    fn restore_gesture_upgrades_recipe_and_reset_participates_in_history() {
        let ctx = egui::Context::default();
        let mut t = Tool {
            enabled: true,
            radius: 25.,
            operation: Some(Operation::Restore),
            ..Default::default()
        };
        let mut p = project();
        let before = p.clone();
        let pos = egui::pos2(200., 200.);
        frame(&ctx, &mut t, &mut p, vec![]);
        frame(
            &ctx,
            &mut t,
            &mut p,
            vec![egui::Event::PointerMoved(pos), pointer(pos, true)],
        );
        frame(&ctx, &mut t, &mut p, vec![pointer(pos, false)]);
        let edits = p.topology_edits.as_ref().unwrap();
        assert_eq!(edits.engine, RESTORE_VERSION);
        assert_eq!(edits.strokes[0].operation, Operation::Restore);
        let profile = mapper_studio::profile::Profile::new("Restored".into(), p.clone());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("restore.rec");
        profile.save(&path).unwrap();
        assert_eq!(mapper_studio::profile::Profile::load(&path).unwrap().recipe, {
            let mut current = p.clone();
            mapper_studio::generation_defaults::apply(&mut current);
            current
        });
        let mut history = mapper_studio::History::default();
        let painted = p.clone();
        t.reset_selected(&mut p, 4);
        assert!(Arc::ptr_eq(
            p.topology_edits.as_ref().unwrap(),
            painted.topology_edits.as_ref().unwrap(),
        ));
        t.reset_selected(&mut p, 2);
        assert!(p.topology_edits.is_none());
        assert_eq!(p, before);
        history.record(painted.clone(), &p);
        history.undo(&mut p);
        assert_eq!(p, painted);
        history.redo(&mut p);
        assert_eq!(p, before);
    }
    #[test]
    fn escape_discards_the_pending_stroke() {
        let ctx = egui::Context::default();
        let mut t = Tool {
            enabled: true,
            radius: 25.,
            ..Default::default()
        };
        let mut p = project();
        let before = p.clone();
        let pos = egui::pos2(200., 200.);
        frame(&ctx, &mut t, &mut p, vec![]);
        frame(
            &ctx,
            &mut t,
            &mut p,
            vec![egui::Event::PointerMoved(pos), pointer(pos, true)],
        );
        frame(
            &ctx,
            &mut t,
            &mut p,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
        );
        frame(&ctx, &mut t, &mut p, vec![pointer(pos, false)]);
        assert_eq!(p, before);
    }
    #[test]
    fn tool_rows_keep_the_map_origin_stable_when_status_and_layer_controls_change() {
        for width in [600., 1000.] {
            let ctx = egui::Context::default();
            let mut tool = Tool {
                enabled: true,
                radius: 25.,
                ..Default::default()
            };
            let mut project = project();
            let measure = |tool: &mut Tool, project: &mut Project| {
                let mut y = 0.;
                let mut out = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(width, 700.))),
                        ..Default::default()
                    },
                    |ui| {
                        tool.controls(ui, project, true, 1);
                        y = ui.cursor().top();
                    },
                );
                out.textures_delta.clear();
                y
            };
            let initial = measure(&mut tool, &mut project);
            project.topology_edits = Some(Arc::new(Edits {
                engine: VERSION.into(),
                source_sha256: "b".repeat(64),
                size: project.size,
                enabled: true,
                strokes: vec![BrushStroke {
                    shape: Default::default(),
                    operation: Operation::Add,
                    mask: 2,
                    radius: 25.,
                    points: vec![[0., 0.]],
                }],
            }));
            tool.message = "Topology stroke saved. Undo restores the previous layer.".into();
            let after = measure(&mut tool, &mut project);
            assert_eq!(initial, after);
            tool.draft = Some((
                project.clone(),
                project.topology_edits.as_ref().unwrap().strokes[0].clone(),
            ));
            tool.message.clear();
            assert_eq!(initial, measure(&mut tool, &mut project));
        }
    }

    #[test]
    fn multilayer_erase_square_is_one_undoable_gesture_and_source_loss_cancels() {
        let ctx = egui::Context::default();
        let mut tool = Tool {
            enabled: true,
            radius: 25.,
            operation: Some(Operation::Remove),
            shape: Shape::Square,
            mask: Some(3),
            ..Default::default()
        };
        let mut project = project();
        let before = project.clone();
        let pos = egui::pos2(200., 200.);
        frame(&ctx, &mut tool, &mut project, vec![]);
        frame(
            &ctx,
            &mut tool,
            &mut project,
            vec![egui::Event::PointerMoved(pos), pointer(pos, true)],
        );
        frame(&ctx, &mut tool, &mut project, vec![pointer(pos, false)]);
        let edits = project.topology_edits.as_ref().unwrap();
        assert_eq!(edits.strokes.len(), 1);
        assert_eq!(edits.engine, BRUSH_VERSION);
        assert_eq!(edits.strokes[0].mask, 3);
        assert_eq!(edits.strokes[0].operation, Operation::Remove);
        assert_eq!(edits.strokes[0].shape, Shape::Square);
        let mut history = mapper_studio::History::default();
        history.record(before.clone(), &project);
        history.undo(&mut project);
        assert_eq!(project, before);
        history.redo(&mut project);
        let accepted = project.clone();
        frame(&ctx, &mut tool, &mut project, vec![pointer(pos, true)]);
        assert!(tool.draft.is_some());
        // Simulate a binding change while the gesture still has the same map.
        tool.draft_source = Some(("c".repeat(64), 3));
        frame(&ctx, &mut tool, &mut project, vec![pointer(pos, false)]);
        assert_eq!(project, accepted);
        assert!(tool.draft.is_none());
        tool.mask = Some(0);
        frame(&ctx, &mut tool, &mut project, vec![pointer(pos, true)]);
        frame(&ctx, &mut tool, &mut project, vec![pointer(pos, false)]);
        assert_eq!(project, accepted);
    }

    #[test]
    fn topology_layer_menu_selects_multiple_names_and_renders_both_themes() {
        mod snapshot {
            include!("../examples/support/egui_snapshot.rs");
        }
        fn text_center(shapes: &[egui::epaint::ClippedShape], label: &str) -> Pos2 {
            fn find(shape: &egui::Shape, label: &str) -> Option<Pos2> {
                match shape {
                    egui::Shape::Text(t) if t.galley.text() == label => {
                        Some(t.pos + t.galley.size() * 0.5)
                    }
                    egui::Shape::Vec(children) => children.iter().find_map(|s| find(s, label)),
                    _ => None,
                }
            }
            shapes
                .iter()
                .find_map(|s| find(&s.shape, label))
                .unwrap_or_else(|| panic!("Missing widget {label}"))
        }
        for dark in [true, false] {
            for width in [600, 1000] {
                let ctx = egui::Context::default();
                ctx.set_theme(if dark {
                    egui::Theme::Dark
                } else {
                    egui::Theme::Light
                });
                ctx.global_style_mut(|style| style.animation_time = 0.);
                let mut tool = Tool {
                    enabled: true,
                    radius: 25.,
                    ..Default::default()
                };
                let mut project = project();
                let before = project.clone();
                let flags = vec![
                    ("Field".into(), 1),
                    ("PlayerSpawn".into(), 2),
                    ("Cliff".into(), 4),
                ];
                let mut groups = vec![mapper_studio::topology_presets::Preset {
                    name: "My layers".into(),
                    layers: vec!["Field".into(), "PlayerSpawn".into()],
                }];
                let mut textures = snapshot::Textures::default();
                let render = |tool: &mut Tool,
                              project: &mut Project,
                              groups: &mut Vec<mapper_studio::topology_presets::Preset>,
                              textures: &mut snapshot::Textures,
                              events: Vec<egui::Event>| {
                    let mut out = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(
                                Pos2::ZERO,
                                egui::vec2(width as f32, 700.),
                            )),
                            focused: true,
                            events,
                            ..Default::default()
                        },
                        |ui| {
                            tool.controls_with_layers(ui, project, true, 0, &flags, groups);
                            let available = ui.available_rect_before_wrap();
                            ui.painter().rect_filled(available, 0., MAP_BACKGROUND);
                        },
                    );
                    textures.update(&out.textures_delta);
                    out.textures_delta.clear();
                    out
                };
                for _ in 0..2 {
                    render(&mut tool, &mut project, &mut groups, &mut textures, vec![]);
                }
                let out = render(&mut tool, &mut project, &mut groups, &mut textures, vec![]);
                let layers = text_center(&out.shapes, "Layers (1)");
                render(
                    &mut tool,
                    &mut project,
                    &mut groups,
                    &mut textures,
                    vec![egui::Event::PointerMoved(layers), pointer(layers, true)],
                );
                render(
                    &mut tool,
                    &mut project,
                    &mut groups,
                    &mut textures,
                    vec![pointer(layers, false)],
                );
                let out = render(&mut tool, &mut project, &mut groups, &mut textures, vec![]);
                let spawn = text_center(&out.shapes, "PlayerSpawn");
                render(
                    &mut tool,
                    &mut project,
                    &mut groups,
                    &mut textures,
                    vec![egui::Event::PointerMoved(spawn), pointer(spawn, true)],
                );
                render(
                    &mut tool,
                    &mut project,
                    &mut groups,
                    &mut textures,
                    vec![pointer(spawn, false)],
                );
                let out = render(&mut tool, &mut project, &mut groups, &mut textures, vec![]);
                assert_eq!(tool.selection_mask(0), 3);
                assert_eq!(project, before);
                assert!(tool.take_overlay_change());
                assert!(!tool.take_overlay_change());
                if let Some(dir) = std::env::var_os("MAPPER_UI_SNAPSHOT_DIR") {
                    let dir = PathBuf::from(dir);
                    std::fs::create_dir_all(&dir).unwrap();
                    let image = textures.raster(&ctx, out, [width, 700]);
                    mapper_studio::image_export::write_png(
                        &dir.join(format!(
                            "topology-{}-{width}.png",
                            if dark { "dark" } else { "light" }
                        )),
                        &image,
                        || false,
                    )
                    .unwrap();
                }
                // The checkbox list must stay open across multiple selections.
                let out = render(&mut tool, &mut project, &mut groups, &mut textures, vec![]);
                let cliff = text_center(&out.shapes, "Cliff");
                render(
                    &mut tool,
                    &mut project,
                    &mut groups,
                    &mut textures,
                    vec![egui::Event::PointerMoved(cliff), pointer(cliff, true)],
                );
                render(
                    &mut tool,
                    &mut project,
                    &mut groups,
                    &mut textures,
                    vec![pointer(cliff, false)],
                );
                let out = render(&mut tool, &mut project, &mut groups, &mut textures, vec![]);
                assert_eq!(tool.selection_mask(0), 7);
                let group_button = text_center(&out.shapes, "Groups");
                render(
                    &mut tool,
                    &mut project,
                    &mut groups,
                    &mut textures,
                    vec![
                        egui::Event::PointerMoved(group_button),
                        pointer(group_button, true),
                    ],
                );
                render(
                    &mut tool,
                    &mut project,
                    &mut groups,
                    &mut textures,
                    vec![pointer(group_button, false)],
                );
                let out = render(&mut tool, &mut project, &mut groups, &mut textures, vec![]);
                let group = text_center(&out.shapes, "My layers");
                render(
                    &mut tool,
                    &mut project,
                    &mut groups,
                    &mut textures,
                    vec![egui::Event::PointerMoved(group), pointer(group, true)],
                );
                render(
                    &mut tool,
                    &mut project,
                    &mut groups,
                    &mut textures,
                    vec![pointer(group, false)],
                );
                let out = render(&mut tool, &mut project, &mut groups, &mut textures, vec![]);
                assert_eq!(tool.selection_mask(0), 3);
                let group_button = text_center(&out.shapes, "Groups");
                render(
                    &mut tool,
                    &mut project,
                    &mut groups,
                    &mut textures,
                    vec![
                        egui::Event::PointerMoved(group_button),
                        pointer(group_button, true),
                    ],
                );
                render(
                    &mut tool,
                    &mut project,
                    &mut groups,
                    &mut textures,
                    vec![pointer(group_button, false)],
                );
                let out = render(&mut tool, &mut project, &mut groups, &mut textures, vec![]);
                let name = text_center(&out.shapes, "Group name");
                render(
                    &mut tool,
                    &mut project,
                    &mut groups,
                    &mut textures,
                    vec![egui::Event::PointerMoved(name), pointer(name, true)],
                );
                render(
                    &mut tool,
                    &mut project,
                    &mut groups,
                    &mut textures,
                    vec![pointer(name, false)],
                );
                render(
                    &mut tool,
                    &mut project,
                    &mut groups,
                    &mut textures,
                    vec![egui::Event::Text("My spawn layers".into())],
                );
                let out = render(&mut tool, &mut project, &mut groups, &mut textures, vec![]);
                let save = text_center(&out.shapes, "Save selected layers");
                render(
                    &mut tool,
                    &mut project,
                    &mut groups,
                    &mut textures,
                    vec![egui::Event::PointerMoved(save), pointer(save, true)],
                );
                render(
                    &mut tool,
                    &mut project,
                    &mut groups,
                    &mut textures,
                    vec![pointer(save, false)],
                );
                assert_eq!(groups.len(), 2);
                assert_eq!(groups[1].name, "My spawn layers");
                assert_eq!(
                    mapper_studio::topology_presets::resolve(&groups[1].layers, &flags),
                    Ok(3)
                );
                assert_eq!(project, before);
                tool.layers = Some(vec!["Unavailable".into()]);
                render(&mut tool, &mut project, &mut groups, &mut textures, vec![]);
                assert_eq!(tool.selection_mask(0), 0);
            }
        }
    }
}
