use super::*;

const ROW: f32 = 25.;
const COLUMN_HEADER: f32 = 19.;
const GRIP: f32 = 20.;

#[derive(Clone, Copy)]
struct Drag {
    id: u64,
    offset: f32,
}

#[derive(Clone, Copy)]
struct Columns {
    seed: f32,
    recipe: f32,
    status: f32,
    stage: f32,
    elapsed: f32,
    actions: f32,
}

impl Columns {
    fn new(width: f32, has_recipes: bool) -> Self {
        let elapsed = if width >= 500. { 58. } else { 0. };
        let actions = if width >= 400. { 119. } else { 78. };
        let status = if width >= 310. { 86. } else { 63. };
        let seed = if width >= 400. { 111. } else { 80. };
        let recipe = if has_recipes && width >= 495. { (width * 0.15).clamp(75., 140.) } else { 0. };
        Self {
            seed,
            recipe,
            status,
            stage: (width - GRIP - seed - recipe - status - elapsed - actions).max(0.),
            elapsed,
            actions,
        }
    }
}

#[derive(Clone, Copy)]
enum Glyph {
    Play,
    Pause,
    Grip,
    More,
    Filter,
    Chevron,
}

impl Glyph {
    fn paint(self, ui: &egui::Ui, at: Pos2, color: Color32) {
        let p = ui.painter();
        match self {
            Self::Play => {
                p.add(egui::Shape::convex_polygon(
                    vec![
                        at + Vec2::new(-3., -5.),
                        at + Vec2::new(5., 0.),
                        at + Vec2::new(-3., 5.),
                    ],
                    color,
                    Stroke::NONE,
                ));
            }
            Self::Pause => {
                for x in [-2.3, 2.3] {
                    p.rect_filled(
                        Rect::from_center_size(at + Vec2::new(x, 0.), Vec2::new(2., 10.)),
                        1.,
                        color,
                    );
                }
            }
            Self::Grip => {
                for x in [-2., 2.] {
                    for y in [-4., 0., 4.] {
                        p.circle_filled(at + Vec2::new(x, y), 1., color);
                    }
                }
            }
            Self::Filter => {
                for (length, y) in [(5., -3.), (3., 0.), (1., 3.)] {
                    p.hline(
                        at.x - length..=at.x + length,
                        at.y + y,
                        Stroke::new(1., color),
                    );
                }
            }
            Self::Chevron => {
                p.line_segment(
                    [at + Vec2::new(-3., -1.5), at + Vec2::new(0., 1.5)],
                    Stroke::new(1., color),
                );
                p.line_segment(
                    [at + Vec2::new(0., 1.5), at + Vec2::new(3., -1.5)],
                    Stroke::new(1., color),
                );
            }
            Self::More => {
                for x in [-4., 0., 4.] {
                    p.circle_filled(at + Vec2::new(x, 0.), 1., color);
                }
            }
        }
    }
}

fn transport(
    ui: &mut egui::Ui,
    id: egui::Id,
    glyph: Glyph,
    title: &str,
    enabled: bool,
) -> egui::Response {
    let response = ui.add_enabled(
        enabled,
        egui::Button::new("")
            .frame(false)
            .min_size(Vec2::new(23., 22.)),
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, title));
    let visuals = ui.visuals();
    let fill = visuals
        .weak_text_color()
        .lerp_to_gamma(visuals.panel_fill, if enabled { 0.22 } else { 0.68 });
    let hover = ui.ctx().animate_bool_with_time(
        id,
        enabled && (response.hovered() || response.has_focus()),
        ui.style().animation_time.min(0.12),
    );
    glyph.paint(
        ui,
        response.rect.center(),
        fill.lerp_to_gamma(visuals.text_color(), hover),
    );
    if response.has_focus() {
        ui.painter().rect_stroke(
            response.rect.shrink(2.),
            4.,
            visuals.selection.stroke,
            egui::StrokeKind::Inside,
        );
    }
    response.on_hover_text(title).on_disabled_hover_text(title)
}

fn cell(
    ui: &mut egui::Ui,
    width: f32,
    height: f32,
    text: impl Into<egui::WidgetText>,
) -> egui::Response {
    ui.allocate_ui_with_layout(
        Vec2::new(width, height),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_size(Vec2::new(width, height));
            ui.add(egui::Label::new(text).truncate())
        },
    )
    .inner
}

fn label(ui: &mut egui::Ui, width: f32, text: impl Into<egui::WidgetText>) -> egui::Response {
    cell(ui, width, ROW, text)
}

fn seed_button(
    ui: &mut egui::Ui,
    item: &QueueItem,
    width: f32,
    can_use_seed: bool,
    active_recipe_name: &str,
) -> Option<QueueAction> {
    let mut seed = item.recipe.master_seed;
    if item.origin.is_none() {
        seed = item.label.strip_prefix("Seed ")
            .and_then(|label| label.split_whitespace().next()?.parse().ok())
            .unwrap_or(seed);
    }
    if seed == 0 {
        seed = item.recipe.seeds[0].value;
    }
    let recipe_name = item.origin.as_ref().map(|origin| origin.name.as_str());
    let label = seed.to_string();
    let display = match item.label.split_once(" · ") {
        Some((_, id)) if id.parse::<u64>() == Ok(item.id) => format!("{label} · {id}"),
        _ => label.clone(),
    };
    let response = ui.add_sized(
        [width - 2., 22.],
        egui::Button::new(egui::Atoms::new((
            egui::RichText::new(&display).monospace().atom_shrink(true),
            egui::Atom::grow(),
        )))
        .frame(false)
        .truncate(),
    ).on_hover_cursor(egui::CursorIcon::PointingHand);
    ui.add_space(2.);
    let mut action = None;
    let copied_id = response.id.with("seed-feedback");
    let now = ui.input(|input| input.time);
    if response.clicked() {
        let control = ui.input(|input| input.modifiers.ctrl);
        if control && can_use_seed && (1..=mapper_studio::MAX_SEED).contains(&seed) {
            action = Some(QueueAction::UseSeed(seed));
            ui.data_mut(|data| data.insert_temp(copied_id, (now + 1.3, format!("Seed set in {active_recipe_name}"))));
        } else if !control {
            ui.ctx().copy_text(label);
            ui.data_mut(|data| data.insert_temp(copied_id, (now + 1.3, String::from("Seed copied"))));
        }
    }
    response.context_menu(|ui| {
        if ui.button("Open map").clicked() {
            action = Some(QueueAction::OpenMap(item.id));
            ui.close();
        }
    });
    if let Some((until, hint)) = ui.data(|data| data.get_temp::<(f64, String)>(copied_id))
        && until > now
    {
        egui::Tooltip::for_widget(&response).show(|ui| ui.label(hint));
        ui.ctx().request_repaint_after(Duration::from_secs_f64(until - now));
    } else {
        let hint = format!("Click to copy seed. Ctrl-click sets the master seed in {active_recipe_name}.");
        response.on_hover_text(recipe_name.map_or_else(|| hint.clone(), |name| format!("{name}\n{hint}")));
    }
    action
}

fn grip(ui: &mut egui::Ui, id: u64, y: f32) -> Option<QueueAction> {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(GRIP, ROW), Sense::click_and_drag());
    let response = response
        .on_hover_cursor(egui::CursorIcon::Grab)
        .on_hover_text("Drag to reorder. Use Up or Down when focused.");
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Reorder map"));
    Glyph::Grip.paint(ui, rect.center(), ui.visuals().weak_text_color());
    if response.drag_started()
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let press = ui
            .input(|input| input.pointer.press_origin())
            .unwrap_or(pointer);
        response.dnd_set_drag_payload(Drag {
            id,
            offset: press.y - y,
        });
    }
    if response.has_focus() {
        ui.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                response.id,
                egui::EventFilter {
                    vertical_arrows: true,
                    ..Default::default()
                },
            )
        });
        for (key, direction) in [(egui::Key::ArrowUp, -1), (egui::Key::ArrowDown, 1)] {
            if ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, key)) {
                return Some(QueueAction::StepWaiting { id, direction });
            }
        }
    }
    None
}

impl Studio {
    pub(crate) fn queue(&mut self, ui: &mut egui::Ui) {
        let waiting = self.generation.waiting_count();
        let running = self
            .generation
            .queue
            .iter()
            .filter(|job| job.state == State::Running)
            .count();
        let filter = self.generation.queue_filter;
        let mut action = None;
        let paused_count = self
            .generation
            .queue
            .iter()
            .filter(|item| item.state == State::Running && self.queue_job_paused(item.id))
            .count();
        ui.horizontal_wrapped(|ui| {
            let available = ui.available_width();
            if available >= 495. {
                let caption = if paused_count == 0 {
                    format!("{running} running, {waiting} waiting")
                } else {
                    format!(
                        "{} running, {paused_count} paused, {waiting} waiting",
                        running - paused_count
                    )
                };
                let text = if running + waiting > 0 {
                    caption.as_str()
                } else {
                    ""
                };
                cell(ui, available - 350., ROW, egui::RichText::new(text).weak())
                    .on_hover_text(text);
            }
            let start = ui.add_enabled(
                waiting > 0 && !self.generation.queue_paused,
                egui::Button::new(egui::RichText::new("Start all").color(Color32::WHITE))
                    .fill(Color32::from_rgb(27, 88, 163))
                    .corner_radius(6.)
                    .min_size(Vec2::new(65., 23.)),
            );
            if start
                .on_hover_text("Run waiting maps as generation slots become available.")
                .clicked()
            {
                action = Some(QueueAction::StartAll);
            }
            let paused = self.generation.queue_paused;
            let title = if paused { "Resume all" } else { "Pause all" };
            let content = egui::Atoms::new((
                egui::Atom::custom(egui::Id::new("all-pause-glyph"), Vec2::splat(13.)),
                title,
            ));
            let response = ui.add_enabled(
                paused || running + waiting > 0,
                egui::Button::new(content).frame(false),
            );
            let visuals = ui.visuals();
            let hover = ui.ctx().animate_bool_with_time(
                response.id.with("tint"),
                response.hovered() || response.has_focus(),
                ui.style().animation_time.min(0.12),
            );
            let idle = visuals
                .weak_text_color()
                .lerp_to_gamma(visuals.panel_fill, 0.22);
            let glyph = if paused { Glyph::Play } else { Glyph::Pause };
            glyph.paint(
                ui,
                Pos2::new(
                    response.rect.left() + ui.spacing().button_padding.x + 6.5,
                    response.rect.center().y,
                ),
                idle.lerp_to_gamma(visuals.text_color(), hover),
            );
            if response.on_hover_text(title).clicked() {
                action = Some(QueueAction::ToggleQueuePause);
            }
            let filter_content = egui::Atoms::new((
                egui::Atom::custom(egui::Id::new("filter-glyph"), Vec2::splat(12.)),
                filter.label(),
                egui::Atom::custom(egui::Id::new("filter-chevron"), Vec2::splat(10.)),
            ));
            let (response, _) = MenuButton::from_button(
                egui::Button::new(filter_content).corner_radius(6.),
            )
            .ui(ui, |ui| {
                for choice in QueueFilter::ALL {
                    if ui
                        .selectable_value(&mut self.generation.queue_filter, choice, choice.label())
                        .clicked()
                    {
                        ui.close();
                    }
                }
            });
            let padding = ui.spacing().button_padding.x;
            let tint = ui.visuals().weak_text_color();
            Glyph::Filter.paint(
                ui,
                Pos2::new(
                    response.rect.left() + padding + 6.,
                    response.rect.center().y,
                ),
                tint,
            );
            Glyph::Chevron.paint(
                ui,
                Pos2::new(
                    response.rect.right() - padding - 5.,
                    response.rect.center().y,
                ),
                tint,
            );
            response.on_hover_text("Filter jobs by status.");
            let actions = self.queue_actions(ui);
            Glyph::Chevron.paint(
                ui,
                Pos2::new(actions.rect.right() - padding - 5., actions.rect.center().y),
                tint,
            );
        });
        let ordered: Vec<_> = [State::Running, State::Waiting]
            .into_iter()
            .flat_map(|state| {
                self.generation
                    .queue
                    .iter()
                    .filter(move |item| item.state == state)
            })
            .chain(
                self.generation
                    .queue
                    .iter()
                    .filter(|item| !matches!(item.state, State::Running | State::Waiting)),
            )
            .filter(|item| filter.includes(item.state))
            .collect();
        if ordered.is_empty() {
            ui.weak(if self.generation.queue.is_empty() {
                "Queue is empty"
            } else {
                "No matching jobs"
            });
            self.apply_queue_action(action, ui.ctx());
            return;
        }
        let width = ui.available_width() - ui.spacing().scroll.allocated_width();
        let columns = Columns::new(width, ordered.iter().any(|item| item.origin.is_some()));
        let (seam, _) = ui.allocate_exact_size(Vec2::new(width, 1.), Sense::hover());
        ui.painter().hline(
            seam.x_range(),
            seam.center().y,
            ui.visuals().widgets.noninteractive.bg_stroke,
        );
        if width >= 260.
            && ui.available_height() >= 2. * ROW + COLUMN_HEADER + ui.spacing().item_spacing.y
        {
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.x = 0.;
                ui.horizontal(|ui| {
                    ui.add_space(GRIP);
                    cell(
                        ui,
                        columns.seed,
                        COLUMN_HEADER,
                        egui::RichText::new("Map seed").weak(),
                    );
                    if columns.recipe > 0. {
                        cell(ui, columns.recipe, COLUMN_HEADER, egui::RichText::new("Recipe").weak());
                    }
                    cell(
                        ui,
                        columns.status,
                        COLUMN_HEADER,
                        egui::RichText::new("Status").weak(),
                    );
                    if columns.stage > 0. {
                        cell(
                            ui,
                            columns.stage,
                            COLUMN_HEADER,
                            egui::RichText::new("Stage").weak(),
                        );
                    }
                    if columns.elapsed > 0. {
                        cell(
                            ui,
                            columns.elapsed,
                            COLUMN_HEADER,
                            egui::RichText::new("Elapsed").weak(),
                        );
                    }
                });
            });
        }
        let (stage, fraction) = self.generation_stage();
        let active = self.generation.run.as_ref().map(|run| run.active_id);
        let next = self
            .generation
            .queue
            .iter()
            .find(|item| item.state == State::Waiting)
            .map(|item| item.id);
        let can_requeue = self.can_requeue();
        let can_use_seed = self.drag_before.is_none();
        let active_recipe_name = self.recipe_tabs.name(self.recipe_tabs.active()).to_owned();
        let can_load = self.can_load_queue_recipe();
        let reveal = self
            .generation
            .queue_focus
            .or_else(|| active.filter(|id| Some(*id) != self.generation.revealed_job));
        let mut visited_reveal = false;
        egui::ScrollArea::vertical()
            .id_salt("queue-table")
            .auto_shrink([false, false])
            .show_viewport(ui, |ui, viewport| {
                let origin = ui.max_rect().min;
                let clip = ui.clip_rect();
                ui.set_min_height(ordered.len() as f32 * ROW);
                let position = |index: usize| origin.y + index as f32 * ROW;
                if let Some(slot) =
                    reveal.and_then(|id| ordered.iter().position(|item| item.id == id))
                {
                    visited_reveal = true;
                    if !viewport.contains(Pos2::new(
                        viewport.center().x,
                        slot as f32 * ROW + ROW * 0.5,
                    )) {
                        ui.scroll_to_rect(
                            Rect::from_min_size(
                                Pos2::new(origin.x, position(slot)),
                                Vec2::new(width, ROW),
                            ),
                            Some(egui::Align::Center),
                        );
                    }
                }
                let moving = egui::DragAndDrop::payload::<Drag>(ui.ctx())
                    .zip(ui.ctx().pointer_latest_pos())
                    .and_then(|(drag, p)| {
                        let from = ordered
                            .iter()
                            .position(|item| item.id == drag.id && item.state == State::Waiting)?;
                        let first = ordered
                            .iter()
                            .position(|item| item.state == State::Waiting)?;
                        let last = ordered
                            .iter()
                            .rposition(|item| item.state == State::Waiting)?;
                        (clip.expand(2.).contains(p)).then(|| {
                            let target = (((p.y - drag.offset - origin.y) / ROW).round() as usize)
                                .clamp(first, last);
                            (from, target, p.y - drag.offset)
                        })
                    });
                if let Some((from, to, _)) = moving {
                    if ui.input(|input| input.pointer.primary_released()) {
                        action = Some(QueueAction::MoveWaiting {
                            id: ordered[from].id,
                            target: ordered[to].id,
                        });
                        egui::DragAndDrop::clear_payload(ui.ctx());
                    } else {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                        if let Some(pointer) = ui.ctx().pointer_latest_pos() {
                            let edge = 18_f32.min(clip.height() * 0.25).max(1.);
                            let upper = ((clip.top() + edge - pointer.y) / edge).clamp(0., 1.);
                            let lower = ((pointer.y - clip.bottom() + edge) / edge).clamp(0., 1.);
                            let delta = (upper - lower) * 340. * ui.input(|i| i.stable_dt);
                            let can_scroll = (delta > 0. && viewport.top() > 0.)
                                || (delta < 0. && viewport.bottom() < ordered.len() as f32 * ROW);
                            if can_scroll {
                                ui.scroll_with_delta_animation(
                                    Vec2::new(0., delta),
                                    egui::style::ScrollAnimation::none(),
                                );
                                ui.ctx().request_repaint();
                            }
                        }
                    }
                }
                let mut row = |slot: usize, dragged: bool| {
                    let item = ordered[slot];
                    let projected = match moving {
                        Some((from, to, _)) if from < slot && slot <= to => slot - 1,
                        Some((from, to, _)) if to <= slot && slot < from => slot + 1,
                        _ => slot,
                    };
                    let motion = ui.id().with(("queue-row", item.id));
                    let offset = if dragged {
                        let y = moving
                            .unwrap()
                            .2
                            .clamp(clip.top(), (clip.bottom() - ROW).max(clip.top()));
                        ui.ctx().animate_value_with_time(motion, y - origin.y, 0.)
                    } else {
                        ui.ctx().animate_value_with_time(
                            motion,
                            projected as f32 * ROW,
                            ui.style().animation_time.min(0.18),
                        )
                    };
                    let rect = Rect::from_min_size(
                        Pos2::new(origin.x, origin.y + offset),
                        Vec2::new(width, ROW),
                    );
                    if !clip.intersects(rect) {
                        return;
                    }
                    let mut child = ui.new_child(
                        egui::UiBuilder::new()
                            .id(ui.id().with(("job", item.id)))
                            .max_rect(rect)
                            .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    );
                    child.set_clip_rect(clip.intersect(rect));
                    child.spacing_mut().item_spacing.x = 0.;
                    let bg = child.visuals().faint_bg_color;
                    let paused = self.queue_job_paused(item.id);
                    if item.state == State::Running && !paused {
                        child.painter().rect_filled(
                            rect.shrink(0.5),
                            5.,
                            bg.lerp_to_gamma(child.visuals().selection.bg_fill, 0.40),
                        );
                    } else if dragged || child.rect_contains_pointer(rect) {
                        child.painter().rect_filled(rect.shrink(0.5), 5., bg);
                    } else {
                        if projected % 2 == 1 {
                            let stripe = child.visuals().panel_fill.lerp_to_gamma(bg, 0.60);
                            child.painter().rect_filled(rect, 0., stripe);
                        }
                        child.painter().hline(
                            rect.x_range(),
                            rect.bottom(),
                            child.visuals().widgets.noninteractive.bg_stroke,
                        );
                    }
                    if item.state == State::Waiting {
                        if let Some(step) = grip(&mut child, item.id, rect.top()) {
                            action = Some(step);
                        }
                    } else {
                        child.add_space(GRIP);
                    }
                    if let Some(seed) = seed_button(&mut child, item, columns.seed, can_use_seed, &active_recipe_name) {
                        action = Some(seed);
                    }
                    if columns.recipe > 0. {
                        let name = item.origin.as_ref().map_or("", |origin| origin.name.as_str());
                        label(&mut child, columns.recipe, egui::RichText::new(name).weak()).on_hover_text(name);
                    }
                    let caption = match item.state {
                        State::Running if paused => self.queue_pause_caption(item.id),
                        State::Running => "Running",
                        State::Waiting if next == Some(item.id) => "Next",
                        State::Waiting => "Queued",
                        State::Ready => "Ready",
                        State::Incomplete => "Incomplete",
                        State::Failed => "Failed",
                        State::Cancelled => "Cancelled",
                    };
                    let fg = match item.state {
                        State::Running if paused => child.visuals().warn_fg_color,
                        State::Running => child.visuals().hyperlink_color,
                        State::Failed => child.visuals().error_fg_color,
                        State::Incomplete => child.visuals().warn_fg_color,
                        _ => child.visuals().weak_text_color(),
                    };
                    let status_origin = child.next_widget_position();
                    child.painter().circle_filled(
                        Pos2::new(status_origin.x + 3., rect.center().y),
                        2.2,
                        fg,
                    );
                    child.add_space(10.);
                    label(
                        &mut child,
                        columns.status - 10.,
                        egui::RichText::new(caption).color(fg),
                    );
                    if columns.stage > 0. {
                        let detail = match item.state {
                            State::Running if active == Some(item.id) => stage.clone(),
                            State::Running => self.extra_queue_stage(item.id).0,
                            State::Failed | State::Incomplete => {
                                diagnostic_ui::queue_summary(&item.detail)
                            }
                            _ => Cow::Borrowed(""),
                        };
                        let progress = if item.state == State::Running {
                            if active == Some(item.id) {
                                fraction
                            } else {
                                self.extra_queue_stage(item.id).1
                            }
                        } else {
                            None
                        };
                        let progress = progress.filter(|_| columns.stage >= 120.);
                        let reserved = if progress.is_some() { 44. } else { 0. };
                        label(&mut child, columns.stage - reserved, detail.as_ref())
                            .on_hover_text(detail.as_ref());
                        if let Some(value) = progress {
                            let shown = ui.ctx().animate_value_with_time(
                                motion.with("progress"),
                                value.clamp(0., 1.),
                                ui.style().animation_time.min(0.16),
                            );
                            child.add(
                                egui::ProgressBar::new(shown)
                                    .desired_width(reserved - 5.)
                                    .desired_height(5.),
                            );
                            child.add_space(5.);
                        }
                    }
                    if columns.elapsed > 0. {
                        let duration = self.queue_elapsed(item.id);
                        label(
                            &mut child,
                            columns.elapsed,
                            duration.map(elapsed_time).unwrap_or_default(),
                        );
                    }
                    if let Some(selected) = queue_table_actions(
                        &mut child,
                        item,
                        paused,
                        self.generation.queue_paused,
                        self.can_start_queue_job(item.id),
                        can_load,
                        can_requeue,
                        columns.actions,
                    ) {
                        action = Some(selected);
                    }
                };
                for slot in 0..ordered.len() {
                    if moving.is_none_or(|(from, _, _)| from != slot) {
                        row(slot, false);
                    }
                }
                if let Some((from, to, _)) = moving {
                    row(from, true);
                    if from != to {
                        let y = position(to + usize::from(from < to));
                        ui.painter().hline(
                            clip.x_range(),
                            y,
                            Stroke::new(2., ui.visuals().hyperlink_color),
                        );
                    }
                }
            });
        if visited_reveal {
            self.generation.revealed_job = active;
            self.generation.queue_focus = None;
        }
        if running > 0 {
            ui.ctx().request_repaint_after(Duration::from_secs(1));
        }
        self.apply_queue_action(action, ui.ctx());
    }
}

pub(super) fn queue_table_actions(
    ui: &mut egui::Ui,
    item: &QueueItem,
    paused: bool,
    all_paused: bool,
    can_start: bool,
    can_load: bool,
    can_requeue: bool,
    width: f32,
) -> Option<QueueAction> {
    let id = item.id;
    match item.state {
        State::Running => {
            let (glyph, title) = if paused {
                (Glyph::Play, "Resume map")
            } else {
                (Glyph::Pause, "Pause map")
            };
            let help = if all_paused {
                "Resume the queue to control this map"
            } else {
                title
            };
            if transport(ui, ui.id().with("transport"), glyph, help, !all_paused).clicked() {
                Some(QueueAction::TogglePause(id))
            } else {
                ui.add(egui::Button::new("Cancel").frame(false))
                    .clicked()
                    .then_some(QueueAction::CancelMap(id))
            }
        }
        State::Waiting => {
            let title = if can_start {
                "Start this map"
            } else {
                "A generation slot is unavailable"
            };
            if transport(ui, ui.id().with("start"), Glyph::Play, title, can_start).clicked() {
                return Some(QueueAction::StartMap(id));
            }
            let mut action = None;
            let (response, _) = MenuButton::from_button(
                egui::Button::new("")
                    .frame(false)
                    .min_size(Vec2::new(22., 22.)),
            )
            .ui(ui, |ui| {
                if ui.button("Run next").clicked() {
                    action = Some(QueueAction::RunNext(id));
                    ui.close();
                }
                if ui.button("Remove").clicked() {
                    action = Some(QueueAction::CancelMap(id));
                    ui.close();
                }
            });
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Waiting map actions")
            });
            Glyph::More.paint(ui, response.rect.center(), ui.visuals().weak_text_color());
            action
        }
        State::Ready => ui
            .add(egui::Button::new("Open map").frame(false))
            .clicked()
            .then_some(QueueAction::OpenMap(id)),
        State::Cancelled => queue_retry_button(ui, "Requeue", can_requeue)
            .clicked()
            .then_some(QueueAction::Requeue(id)),
        State::Failed | State::Incomplete => {
            let mut action = None;
            let label = if width >= 100. {
                "View issues"
            } else {
                "Issues"
            };
            MenuButton::from_button(egui::Button::new(label).frame(false))
                .config(
                    MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside),
                )
                .ui(ui, |ui| {
                    ui.set_max_width(320.);
                    let summary = diagnostic_ui::summary(&item.detail);
                    if !summary.is_empty() {
                        ui.label(summary.as_ref());
                    }
                    ui.horizontal_wrapped(|ui| {
                        if item.state == State::Incomplete && ui.button("Open map").clicked() {
                            ui.close();
                            action = Some(QueueAction::OpenMap(id));
                        }
                        if item.state == State::Failed
                            && queue_retry_button(ui, "Retry", can_requeue).clicked()
                        {
                            ui.close();
                            action = Some(QueueAction::Requeue(id));
                        }
                        if queue_recipe_button(ui, can_load).clicked() {
                            ui.close();
                            action = Some(QueueAction::LoadRecipe(id));
                        }
                        if ui.button("Save recipe…").clicked() {
                            ui.close();
                            action = Some(QueueAction::SaveRecipe(id));
                        }
                        if !item.detail.is_empty() {
                            diagnostic_ui::copy_error(ui, &item.detail, Some(&item.recipe));
                            diagnostic_ui::copy_details(ui, &item.detail);
                        }
                    });
                    if summary.as_ref() != item.detail {
                        ui.collapsing("Technical report", |ui| {
                            diagnostic_ui::report_preview(ui, &item.detail);
                        });
                    }
                });
            action
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn named_recipe_queue_seed_click_copies_and_ctrl_click_targets_the_recipe() {
        use super::*;
        let ctx = egui::Context::default();
        let mut recipe = Project::default();
        recipe.master_seed = 42781;
        recipe.refresh_inherited_seeds();
        let item = QueueItem {
            id: 17,
            label: "Seed 2617 · 17".into(),
            recipe,
            state: State::Ready,
            detail: String::new(),
            origin: Some(recipe_tabs::RecipeOrigin { id: 2, name: "Island Test".into() }),
        };
        let render = |events| {
            let mut action = None;
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(450., 120.))),
                    events,
                    ..Default::default()
                },
                |ui| {
                    action = seed_button(ui, &item, 120., true, "Vanilla 4K");
                },
            );
            output.textures_delta.clear();
            (action, output)
        };
        render(vec![]);
        let (_, initial) = render(vec![]);
        let seed_pos = initial.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == "42781 · 17" => {
                Some(text.pos + text.galley.rect.center().to_vec2())
            }
            _ => None,
        }).expect("The queue displays the master seed");
        let click = |modifiers| {
            let mut result = None;
            for pressed in [true, false] {
                result = Some(render(vec![
                    egui::Event::ModifiersChanged(modifiers),
                    egui::Event::PointerMoved(seed_pos),
                    egui::Event::PointerButton {
                        pos: seed_pos, button: egui::PointerButton::Primary, pressed, modifiers,
                    },
                ]));
            }
            result.unwrap()
        };
        let (action, copied) = click(egui::Modifiers::NONE);
        assert_eq!(action, None);
        assert!(copied.platform_output.commands.iter().any(|command| matches!(
            command, egui::OutputCommand::CopyText(text) if text == "42781"
        )));
        let control = egui::Modifiers { ctrl: true, command: true, ..egui::Modifiers::NONE };
        let (action, assignment) = click(control);
        assert_eq!(action, Some(QueueAction::UseSeed(42781)));
        assert!(!assignment.platform_output.commands.iter().any(|command| matches!(
            command, egui::OutputCommand::CopyText(_)
        )));
    }

    use super::*;

    #[test]
    fn queue_vertical_grip_reorders_only_waiting_rows() {
        let ctx = egui::Context::default();
        workbench_style::install(&ctx);
        workbench_style::set_reduced_motion(&ctx, true);
        let mut app = crate::tests::fixture(&ctx);
        app.generation.queue = (1..=4)
            .map(|id| QueueItem { origin: None,
                id,
                label: format!("Seed {id}"),
                recipe: app.project.clone(),
                state: if id == 1 {
                    State::Running
                } else {
                    State::Waiting
                },
                detail: String::new(),
            })
            .collect();
        app.generation.run = Some(Run {
            active_id: 1,
            start: 0,
            finished: 0,
            template: None,
            terrain_ready: false,
        });
        let render = |app: &mut Studio, events| {
            let mut frame = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(680., 240.))),
                    events,
                    focused: true,
                    ..Default::default()
                },
                |ui| app.queue(ui),
            );
            frame.textures_delta.clear();
            frame
        };
        render(&mut app, vec![]);
        let frame = render(&mut app, vec![]);
        let dots: Vec<_> = frame
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Circle(dot)
                    if dot.radius == 1.
                        && dot.center.x < GRIP
                        && shape.clip_rect.contains(dot.center) =>
                {
                    Some(dot.center)
                }
                _ => None,
            })
            .collect();
        let grips: Vec<_> = dots
            .chunks_exact(6)
            .map(|dots| dots[0].lerp(dots[5], 0.5))
            .collect();
        assert_eq!(grips.len(), 3);
        assert!(
            grips
                .windows(2)
                .all(|pair| pair[0].y < pair[1].y && pair[0].x == pair[1].x)
        );
        let event = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        render(
            &mut app,
            vec![egui::Event::PointerMoved(grips[2]), event(grips[2], true)],
        );
        render(&mut app, vec![egui::Event::PointerMoved(grips[0])]);
        render(&mut app, vec![]);
        render(&mut app, vec![event(grips[0], false)]);
        let order = app
            .generation
            .queue
            .iter()
            .map(|item| item.id)
            .collect::<Vec<_>>();
        assert_eq!(order, [1, 4, 2, 3]);
        assert_eq!(app.generation.queue[0].state, State::Running);
    }
}
