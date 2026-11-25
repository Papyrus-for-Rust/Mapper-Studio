//! Native workbench surfaces, shared across system light and dark appearances.
use eframe::egui::{self, Color32, CornerRadius, FontId, Margin, Stroke};

pub(super) const PRIMARY_ACTION: Color32 = rgb(12, 99, 188);
pub(super) const SELECTED_MONUMENT: Color32 = rgb(164, 52, 59);
const GENERATE_START: Color32 = rgb(18, 108, 192);
const GENERATE_END: Color32 = rgb(24, 67, 149);
pub(super) const MAP_VIEW_SELECTED: Color32 = rgb(68, 100, 119);
const DOCK_BODY_CORNERS: CornerRadius = CornerRadius { nw: 0, ne: 0, sw: 8, se: 8 };
const DOCK_TAB_CORNERS: CornerRadius = CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 };

pub(super) fn install(ctx: &egui::Context) {
    ctx.all_styles_mut(apply);
    #[cfg(windows)]
    if let Some(windows) = std::env::var_os("WINDIR") {
        use std::io::Read;
        let path = std::path::PathBuf::from(windows).join("Fonts/segoeui.ttf");
        if let Ok(file) = std::fs::File::open(path) {
            let mut bytes = Vec::new();
            if file.take(4 * 1024 * 1024).read_to_end(&mut bytes).is_ok()
                && !bytes.is_empty()
                && bytes.len() < 4 * 1024 * 1024
            {
                let mut fonts = egui::FontDefinitions::default();
                fonts
                    .font_data
                    .insert("mapper-ui".into(), egui::FontData::from_owned(bytes).into());
                fonts
                    .families
                    .entry(egui::FontFamily::Proportional)
                    .or_default()
                    .insert(0, "mapper-ui".into());
                ctx.set_fonts(fonts);
            }
        }
    }
}

pub(super) fn set_reduced_motion(ctx: &egui::Context, reduce: bool) {
    let defaults = egui::Style::default();
    ctx.all_styles_mut(|style| {
        style.animation_time = if reduce { 0. } else { defaults.animation_time };
        style.scroll_animation = if reduce {
            egui::style::ScrollAnimation::none()
        } else {
            defaults.scroll_animation
        };
    });
    ctx.clear_animations();
    ctx.request_repaint();
}

pub(super) fn apply(style: &mut egui::Style) {
    let dark = style.visuals.dark_mode;
    let (background, surface, control, hover, foreground, secondary, border, accent) = if dark {
        (
            rgb(20, 22, 24),
            rgb(29, 31, 33),
            rgb(40, 43, 46),
            rgb(54, 63, 66),
            rgb(235, 237, 237),
            rgb(178, 186, 187),
            rgb(59, 64, 68),
            rgb(119, 201, 207),
        )
    } else {
        (
            rgb(224, 228, 229),
            rgb(243, 245, 245),
            rgb(231, 235, 235),
            rgb(213, 230, 230),
            rgb(27, 35, 36),
            rgb(73, 84, 86),
            rgb(185, 194, 196),
            rgb(24, 103, 111),
        )
    };
    style.spacing.item_spacing = egui::vec2(5., 4.);
    style.spacing.button_padding = egui::vec2(7., 3.);
    style.spacing.interact_size.y = 22.;
    style.spacing.window_margin = Margin::same(12);
    for kind in [egui::TextStyle::Body, egui::TextStyle::Button] {
        style.text_styles.insert(kind, FontId::proportional(13.));
    }
    style
        .text_styles
        .insert(egui::TextStyle::Small, FontId::proportional(12.));
    style
        .text_styles
        .insert(egui::TextStyle::Heading, FontId::proportional(15.));
    let visuals = &mut style.visuals;
    visuals.panel_fill = surface;
    visuals.window_fill = surface;
    visuals.window_corner_radius = CornerRadius::same(9);
    visuals.menu_corner_radius = CornerRadius::same(7);
    visuals.window_stroke = Stroke::new(1., border);
    visuals.extreme_bg_color = background;
    visuals.faint_bg_color = control;
    visuals.weak_text_color = Some(secondary);
    visuals.error_fg_color = if dark {
        rgb(255, 180, 180)
    } else {
        rgb(148, 32, 32)
    };
    visuals.warn_fg_color = if dark {
        rgb(255, 213, 138)
    } else {
        rgb(112, 69, 0)
    };
    visuals.hyperlink_color = accent;
    visuals.indent_has_left_vline = false;
    visuals.collapsing_header_frame = true;
    visuals.selection.bg_fill = if dark {
        rgb(43, 82, 86)
    } else {
        rgb(199, 228, 228)
    };
    visuals.selection.stroke = Stroke::new(1., if dark { foreground } else { rgb(17, 77, 82) });
    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = CornerRadius::same(5);
        widget.fg_stroke = Stroke::new(1., foreground);
        widget.bg_stroke = Stroke::NONE;
        widget.bg_fill = control;
        widget.weak_bg_fill = control;
    }
    let track = if dark { control } else { rgb(222, 230, 230) };
    visuals.widgets.noninteractive.bg_fill = track;
    visuals.widgets.noninteractive.weak_bg_fill = track;
    visuals.widgets.noninteractive.bg_stroke = Stroke::NONE;
    visuals.widgets.hovered.bg_fill = hover;
    visuals.widgets.hovered.weak_bg_fill = hover;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1., accent);
    visuals.widgets.active.bg_fill = visuals.selection.bg_fill;
    visuals.widgets.active.weak_bg_fill = visuals.selection.bg_fill;
    visuals.widgets.active.bg_stroke = Stroke::new(1., accent);
}

pub(super) fn paint_switch(ui: &egui::Ui, response: &egui::Response, center: egui::Pos2, enabled: bool) {
    let track = egui::Rect::from_center_size(center, egui::vec2(30., 16.));
    let fill = if enabled {
        if ui.visuals().dark_mode { Color32::from_rgb(46, 105, 115) }
        else { ui.visuals().selection.bg_fill }
    } else {
        ui.visuals().faint_bg_color
    };
    let border = if response.hovered() || response.has_focus() {
        egui::Stroke::new(1., ui.visuals().hyperlink_color)
    } else {
        ui.visuals().window_stroke
    };
    ui.painter().rect(track, 8, fill, border, egui::StrokeKind::Inside);
    let x = if enabled { track.right() - 8. } else { track.left() + 8. };
    let knob = if enabled && ui.visuals().dark_mode {
        Color32::from_rgb(197, 245, 242)
    } else if enabled {
        ui.visuals().hyperlink_color
    } else {
        ui.visuals().weak_text_color()
    };
    ui.painter().circle_filled(egui::pos2(x, track.center().y), 5.5, knob);
}

pub(super) fn dock(ui: &egui::Ui) -> egui_dock::Style {
    let style = ui.style();
    let mut dock = egui_dock::Style::from_egui(style);
    let visuals = &style.visuals;
    dock.main_surface_border_stroke = Stroke::NONE;
    dock.dock_area_padding = Some(Margin::symmetric(4, 6));
    dock.separator.width = 5.;
    dock.separator.extra_interact_width = 8.;
    dock.separator.extra = 90.;
    dock.separator.color_idle = Color32::TRANSPARENT;
    dock.separator.color_hovered = visuals.hyperlink_color;
    dock.separator.color_dragged = visuals.hyperlink_color;
    dock.tab_bar.height = 27.;
    dock.tab_bar.bg_fill = Color32::TRANSPARENT;
    let outline = visuals.window_stroke.color;
    dock.tab_bar.hline_color = outline;
    dock.tab_bar.inner_margin = Margin::ZERO;
    dock.tab.tab_body.stroke = visuals.window_stroke;
    dock.tab.tab_body.corner_radius = DOCK_BODY_CORNERS;
    dock.tab.tab_body.inner_margin = Margin::same(7);
    dock.tab.tab_body.bg_fill = visuals.panel_fill;
    dock.tab.spacing = 0.;
    dock.tab.hline_below_active_tab_name = false;
    for tab in [
        &mut dock.tab.active,
        &mut dock.tab.focused,
        &mut dock.tab.inactive,
        &mut dock.tab.hovered,
        &mut dock.tab.inactive_with_kb_focus,
        &mut dock.tab.active_with_kb_focus,
        &mut dock.tab.focused_with_kb_focus,
    ] {
        tab.outline_color = Color32::TRANSPARENT;
        tab.corner_radius = DOCK_TAB_CORNERS;
    }
    dock.tab.inactive.bg_fill = Color32::TRANSPARENT;
    dock.tab.inactive.text_color = visuals.weak_text_color.unwrap_or(visuals.text_color());
    dock.tab.active.bg_fill = visuals.panel_fill;
    dock.tab.focused.bg_fill = visuals.panel_fill;
    dock.tab.active.outline_color = outline;
    dock.tab.focused.outline_color = outline;
    dock.tab.active_with_kb_focus.bg_fill = visuals.panel_fill;
    dock.tab.focused_with_kb_focus.bg_fill = visuals.panel_fill;
    dock.tab.hovered.bg_fill = visuals.widgets.hovered.bg_fill;
    dock.tab.inactive_with_kb_focus.outline_color = visuals.hyperlink_color;
    dock.tab.active_with_kb_focus.outline_color = visuals.hyperlink_color;
    dock.tab.focused_with_kb_focus.outline_color = visuals.hyperlink_color;
    dock.buttons.close_all_tabs_bg_fill = Color32::TRANSPARENT;
    // The pointer chooses a drop area directly. Center drops group tabs and
    // edge drops split the panel. Floating is available between panels.
    dock.overlay.overlay_type = egui_dock::OverlayType::HighlightedAreas;
    dock.overlay.feel.center_drop_coverage = 0.66;
    dock.overlay.feel.window_drop_coverage = dock.overlay.feel.center_drop_coverage;
    dock.overlay.feel.fade_hold_time = 0.;
    dock.overlay.feel.max_preference_time = 0.08;
    dock.overlay.surface_fade_opacity = 0.82;
    dock.overlay.selection_stroke_width = 2.;
    let dragging = ui.input(|input| input.pointer.is_decidedly_dragging())
        && ui.ctx().dragged_id().is_some();
    let reveal = ui.ctx().animate_bool_with_time_and_easing(
        ui.id().with("dock-drop-reveal"), dragging, style.animation_time.min(0.14),
        egui::emath::easing::quadratic_out,
    );
    let accent = visuals.hyperlink_color;
    dock.overlay.selection_color = accent.gamma_multiply(0.25 + 0.17 * reveal);
    dock.overlay.hovered_leaf_highlight.stroke = Stroke::new(1., accent.gamma_multiply(0.7));
    dock.overlay.hovered_leaf_highlight.corner_radius = CornerRadius::same(6);
    dock
}

pub(super) fn dock_frame(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::NONE.fill(ui.visuals().extreme_bg_color)
}

pub(super) fn finish_dock_body(ui: &egui::Ui) {
    join_dock_fill_and_outline(ui.ctx(), ui.layer_id(), DOCK_BODY_CORNERS, |rect| {
        rect.contains(ui.cursor().min)
    });
}

pub(super) fn finish_dock_title(response: &egui::Response) {
    let ctx = &response.ctx;
    if let Some(id) = ctx.dragged_id().filter(|id| id.with("dragged") == response.id) {
        let layer = egui::LayerId::new(egui::Order::Tooltip, id);
        join_dock_fill_and_outline(ctx, layer, DOCK_TAB_CORNERS, |_| true);
    } else {
        join_dock_fill_and_outline(ctx, response.layer_id, DOCK_TAB_CORNERS,
            |rect| rect == response.rect);
    }
}

fn join_dock_fill_and_outline(
    ctx: &egui::Context,
    layer: egui::LayerId,
    corners: CornerRadius,
    matches_bounds: impl Fn(egui::Rect) -> bool,
) {
    // egui_dock paints the full fill and an inset outline in two consecutive
    // commands. Give them a single contour so the fill cannot peek past the arc.
    ctx.graphics_mut(|graphics| {
        let Some(paint) = graphics.get_mut(layer) else { return };
        let index = {
            let end = paint.next_idx().0;
            let mut entries = paint.all_entries().enumerate().skip(end.saturating_sub(16));
            let Some(mut previous) = entries.next() else { return };
            entries.filter_map(|next| {
                let (index, entry) = std::mem::replace(&mut previous, next);
                let (_, outline) = next;
                let (egui::Shape::Rect(fill), egui::Shape::Rect(stroke)) =
                    (&entry.shape, &outline.shape) else { return None };
                if fill.corner_radius != corners || stroke.corner_radius != corners
                    || fill.fill == Color32::TRANSPARENT || !fill.stroke.is_empty()
                    || stroke.fill != Color32::TRANSPARENT || stroke.stroke.is_empty()
                    || !matches_bounds(fill.rect)
                {
                    return None;
                }
                let mut joined = stroke.clone();
                joined.fill = fill.fill;
                if corners == DOCK_TAB_CORNERS {
                    // Keep the open lower edge flush with the panel.
                    joined.rect.max.y = fill.rect.bottom();
                }
                Some((index, joined))
            }).last()
        };
        if let Some((index, joined)) = index {
            use egui::layers::ShapeIdx;
            paint.reset_shape(ShapeIdx(index));
            paint.mutate_shape(ShapeIdx(index + 1), |shape| shape.shape = joined.into());
        }
    });
}

pub(super) fn join_dock_tab(response: &egui::Response, leaf: egui::Rect) {
    let rect = response.rect;
    let px = response.ctx.pixels_per_point().recip();
    let y = rect.bottom() - px;
    let visuals = &response.ctx.global_style().visuals;
    let outline = visuals.window_stroke.color;
    let edge = if response.has_focus() { visuals.hyperlink_color } else { outline };
    // egui_dock insets the tab and body strokes by one point, while the
    // strip's one-pixel rule starts at the outer edge of the tab.
    let painter = response.ctx.layer_painter(response.layer_id)
        .with_clip_rect(response.interact_rect.expand2(egui::vec2(px, 2.)));
    let left = rect.left() + 1.5;
    let right = rect.right() - 1.5;
    for x in [left, right] {
        painter.line_segment([egui::pos2(x, rect.bottom() - 3.), egui::pos2(x, y)],
            Stroke::new(1., edge));
    }
    if rect.left() <= leaf.left() + px {
        painter.line_segment([egui::pos2(left, y), egui::pos2(left, rect.bottom() + px)],
            Stroke::new(1., outline));
    } else {
        painter.hline(rect.left() - px..=left, y, Stroke::new(px, outline));
    }
    painter.hline(right..=rect.right() + px, y, Stroke::new(px, outline));
}

#[cfg(test)]
#[test]
fn tab_drag_groups_in_the_center_and_splits_at_an_edge() {
    use egui::{Event, PointerButton, Pos2, RawInput, Rect, pos2, vec2};
    use egui_dock::{DockArea, DockState, NodeIndex, TabViewer};

    #[derive(Default)]
    struct Viewer(Option<Pos2>);
    impl TabViewer for Viewer {
        type Tab = &'static str;
        fn id(&mut self, tab: &mut Self::Tab) -> egui::Id { egui::Id::new(*tab) }
        fn title(&mut self, tab: &mut Self::Tab) -> egui::WidgetText { (*tab).into() }
        fn ui(&mut self, _: &mut egui::Ui, _: &mut Self::Tab) {}
        fn on_tab_button(&mut self, tab: &mut Self::Tab, response: &egui::Response) {
            if *tab == "Selection" { self.0 = Some(response.rect.center()); }
        }
    }
    for split in [false, true] {
        let ctx = egui::Context::default();
        let mut dock_state = DockState::new(vec!["Recipe"]);
        dock_state.main_surface_mut().split_right(NodeIndex::root(), 0.5, vec!["Selection"]);
        let mut viewer = Viewer::default();
        let frame = |events, state: &mut DockState<_>, viewer: &mut Viewer| {
            let mut output = ctx.run_ui(RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(900., 650.))),
                events, ..Default::default()
            }, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    DockArea::new(state).style(dock(ui)).show_inside(ui, viewer);
                });
            });
            output.textures_delta.clear();
        };
        frame(vec![], &mut dock_state, &mut viewer);
        let source = viewer.0.unwrap();
        let original = dock_state.find_tab(&"Recipe").unwrap().node_path();
        let rect = dock_state.leaf(original).unwrap().rect;
        let destination = if split { pos2(rect.left() + 12., rect.center().y) } else { rect.center() };
        let button = |pos, pressed| Event::PointerButton {
            pos, pressed, button: PointerButton::Primary, modifiers: Default::default(),
        };
        for events in [
            vec![Event::PointerMoved(source), button(source, true)],
            vec![Event::PointerMoved(source + vec2(0., 18.))],
            vec![Event::PointerMoved(destination)],
            vec![],
            vec![button(destination, false)],
        ] {
            frame(events, &mut dock_state, &mut viewer);
        }
        let selection = dock_state.find_tab(&"Selection").unwrap();
        let recipe = dock_state.find_tab(&"Recipe").unwrap();
        assert!(selection.surface.is_main(), "The tab should drop into the workspace");
        assert_eq!(selection.node == recipe.node, !split);
        if split {
            frame(vec![], &mut dock_state, &mut viewer);
            let selection_leaf = dock_state.leaf(selection.node_path()).unwrap();
            let recipe_leaf = dock_state.leaf(recipe.node_path()).unwrap();
            assert!(selection_leaf.rect.left() < recipe_leaf.rect.left());
        }
    }
}

pub(super) fn generate_fill(
    ctx: &egui::Context,
    rect: egui::Rect,
    corners: CornerRadius,
    uv_start: f32,
) -> egui::Shape {
    let id = egui::Id::new("mapper-generate-gradient");
    let cached = ctx.data(|data| data.get_temp::<egui::TextureHandle>(id));
    let texture = cached.unwrap_or_else(|| {
        let pixels = (0..128).map(|step| {
            let t = step as f32 / 127.;
            let blend = |a: u8, b: u8| (a as f32 * (1. - t) + b as f32 * t).round() as u8;
            rgb(blend(GENERATE_START.r(), GENERATE_END.r()),
                blend(GENERATE_START.g(), GENERATE_END.g()),
                blend(GENERATE_START.b(), GENERATE_END.b()))
        }).collect();
        let image = egui::ColorImage::new([128, 1], pixels);
        let texture = ctx.load_texture("Generate gradient", image, egui::TextureOptions::LINEAR);
        ctx.data_mut(|data| data.insert_temp(id, texture.clone()));
        texture
    });
    let uv = egui::Rect::from_min_max(egui::pos2(uv_start, 0.), egui::pos2(1., 1.));
    egui::epaint::RectShape::filled(rect, corners, Color32::WHITE)
        .with_texture(texture.id(), uv).into()
}

pub(super) fn map_glass() -> egui::Frame {
    egui::Frame::new()
        .fill(Color32::from_rgba_unmultiplied(39, 67, 86, 205))
        .stroke(Stroke::new(1., Color32::from_white_alpha(71)))
        .corner_radius(8)
        .inner_margin(Margin::symmetric(6, 4))
        .shadow(egui::epaint::Shadow {
            offset: [0, 3], blur: 9, spread: 0, color: Color32::from_black_alpha(70),
        })
}

pub(super) fn map_glass_controls(ui: &mut egui::Ui) {
    let style = ui.style_mut();
    style.visuals = egui::Visuals::dark();
    apply(style);
    let visual = &mut style.visuals;
    visual.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    visual.widgets.hovered.weak_bg_fill = Color32::from_white_alpha(24);
    visual.widgets.active.weak_bg_fill = Color32::from_white_alpha(34);
    visual.widgets.open.weak_bg_fill = Color32::from_white_alpha(34);
    visual.selection.bg_fill = MAP_VIEW_SELECTED;
}

pub(super) const SETTINGS_BUTTON_WIDTH: f32 = 26.;

pub(super) fn settings_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let response = ui.add_sized(
        [SETTINGS_BUTTON_WIDTH, ui.spacing().interact_size.y], egui::Button::new("").frame(false),
    );
    response.widget_info(|| egui::WidgetInfo::labeled(
        egui::WidgetType::Button, response.enabled(), label,
    ));
    let center = response.rect.center();
    let stroke = Stroke::new(1.2, ui.style().interact(&response).text_color());
    let outline = (0..32).map(|index| {
        let angle = (index as f32 + 0.5) * std::f32::consts::TAU / 32.;
        let radius = if matches!(index % 4, 1 | 2) { 7. } else { 5.5 };
        center + egui::vec2(angle.cos(), angle.sin()) * radius
    }).collect();
    ui.painter().add(egui::Shape::closed_line(outline, stroke));
    ui.painter().circle_stroke(center, 2.25, stroke);
    if response.has_focus() {
        ui.painter().rect_stroke(response.rect.shrink(1.), 5,
            Stroke::new(1., ui.visuals().hyperlink_color), egui::StrokeKind::Inside);
    }
    response.on_hover_text(label)
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(color: Color32) -> f32 {
        let linear = [color.r(), color.g(), color.b()].map(|value| {
            let value = value as f32 / 255.;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        });
        linear[0] * 0.2126 + linear[1] * 0.7152 + linear[2] * 0.0722
    }

    #[test]
    fn warning_and_error_text_meet_body_contrast_on_workbench_surfaces() {
        for dark in [false, true] {
            let mut style = egui::Style {
                visuals: if dark {
                    egui::Visuals::dark()
                } else {
                    egui::Visuals::light()
                },
                ..Default::default()
            };
            apply(&mut style);
            let visuals = &style.visuals;
            for text in [visuals.error_fg_color, visuals.warn_fg_color] {
                for background in [
                    visuals.extreme_bg_color,
                    visuals.panel_fill,
                    visuals.faint_bg_color,
                    visuals.widgets.hovered.bg_fill,
                    visuals.selection.bg_fill,
                ] {
                    let a = luminance(text);
                    let b = luminance(background);
                    let contrast = (a.max(b) + 0.05) / (a.min(b) + 0.05);
                    assert!(
                        contrast >= 4.5,
                        "{text:?} on {background:?} has contrast {contrast}"
                    );
                }
            }
        }
    }
}
