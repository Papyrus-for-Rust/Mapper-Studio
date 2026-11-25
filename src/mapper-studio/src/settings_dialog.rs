//! Shared chrome for Mapper's two settings dialogs.
use super::workbench_style;
use eframe::egui::{
    self, Align, Color32, FontId, Layout, Rect, RichText, Sense, Stroke, Vec2, pos2, vec2,
};

#[derive(Clone, Copy, Debug, Hash)]
pub(super) enum Icon {
    Globe,
    Seed,
    Terrain,
    Biomes,
    Monument,
    Water,
    Road,
    Rail,
    Objects,
    Pin,
    Rules,
    Output,
    Folder,
    Performance,
    Appearance,
    Viewport,
    Updates,
    Close,
}

pub(super) fn show<Page: Copy + Eq + std::hash::Hash + std::fmt::Debug>(
    ctx: &egui::Context,
    id: &'static str,
    title: &str,
    pages: impl IntoIterator<Item = (Page, &'static str, Icon)>,
    mut selected: Page,
    body: impl FnOnce(&mut egui::Ui, Page),
) -> (Page, bool) {
    let available = ctx.content_rect().size();
    let size = (available - Vec2::splat(36.)).clamp(Vec2::splat(1.), vec2(1000., 712.));
    let compact = size.x < 730.;
    let sidebar = (if compact { 182_f32 } else { 224. }).min(size.x * 0.39);
    let header = if compact { 44. } else { 56. };
    let footer = if compact { 54. } else { 70. };
    let gutter = if compact { 15. } else { 20. };
    let visuals = ctx.style_of(ctx.theme()).visuals.clone();
    let modal = workbench_modal(ctx, egui::Id::new(id), 0.).show(ctx, |ui| {
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let content_top = rect.top() + header;
        let content_bottom = (rect.bottom() - footer).max(content_top);
        let split_x = rect.left() + sidebar;
        let title_rect = Rect::from_min_max(rect.min, pos2(rect.right(), content_top));
        let nav_rect = Rect::from_min_max(
            pos2(rect.left(), content_top),
            pos2(split_x, content_bottom),
        );
        let body_rect =
            Rect::from_min_max(nav_rect.right_top(), pos2(rect.right(), content_bottom));
        let footer_rect = Rect::from_min_max(pos2(rect.left(), content_bottom), rect.max);
        let edge = visuals.window_stroke;

        ui.painter().rect_filled(
            title_rect,
            egui::CornerRadius {
                nw: 11,
                ne: 11,
                sw: 0,
                se: 0,
            },
            visuals.faint_bg_color,
        );
        ui.painter()
            .rect_filled(nav_rect, 0., visuals.extreme_bg_color);
        for y in [content_top, content_bottom] {
            ui.painter().hline(rect.x_range(), y, edge);
        }
        ui.painter()
            .vline(split_x, content_top..=content_bottom, edge);

        let mut title_ui = child(
            ui,
            "title",
            title_rect.shrink2(vec2(46., 0.)),
            Layout::centered_and_justified(egui::Direction::LeftToRight),
        );
        title_ui.add(egui::Label::new(RichText::new(title).size(20.)).truncate());
        let close_rect =
            Rect::from_center_size(title_rect.right_center() - vec2(23., 0.), Vec2::splat(32.));
        let mut close_ui = child(
            ui,
            "close",
            close_rect,
            Layout::centered_and_justified(egui::Direction::LeftToRight),
        );
        let close = close_ui
            .add_sized(
                close_rect.size(),
                egui::Button::new("").frame_when_inactive(false),
            )
            .on_hover_text("Close");
        close.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Close"));
        Icon::Close.paint(
            &close_ui,
            close.rect.center(),
            17.,
            close_ui.visuals().text_color(),
        );

        let mut nav = child(
            ui,
            "navigation",
            nav_rect.shrink2(vec2(10., 12.)),
            Layout::top_down(Align::Min),
        );
        let nav_height = nav.available_height();
        let row_height = if compact || nav_height < 510. {
            32.
        } else {
            40.
        };
        nav.spacing_mut().item_spacing.y = 3.;
        let nav_changed = egui::ScrollArea::vertical()
            .id_salt("topics")
            .max_height(nav_height)
            .show(&mut nav, |ui| {
                let mut changed = false;
                for (page, label, icon) in pages {
                    if page_button(ui, row_height, compact, selected == page, label, icon).clicked()
                    {
                        changed |= selected != page;
                        selected = page;
                    }
                }
                changed
            })
            .inner;

        let mut content = child(
            ui,
            "body",
            body_rect.shrink2(vec2(gutter, 16.)),
            Layout::top_down(Align::Min),
        );
        let style = content.style_mut();
        style.spacing.item_spacing = vec2(7., 8.);
        style.spacing.interact_size.y = 28.;
        for key in [egui::TextStyle::Body, egui::TextStyle::Button] {
            style.text_styles.insert(key, FontId::proportional(15.));
        }
        style
            .text_styles
            .insert(egui::TextStyle::Heading, FontId::proportional(21.));
        let height = content.available_height();
        egui::ScrollArea::vertical()
            .id_salt(("page", selected))
            .max_height(height)
            .auto_shrink([false, false])
            .show(&mut content, |ui| body(ui, selected));
        // Leave focus on the chosen tab so keyboard users can continue
        // through the navigation or into the newly displayed controls.
        if nav_changed {
            ctx.request_repaint();
        }

        let done_rect =
            Rect::from_center_size(footer_rect.right_center() - vec2(58., 0.), vec2(80., 38.));
        let mut footer = child(
            ui,
            "footer",
            done_rect,
            Layout::centered_and_justified(egui::Direction::LeftToRight),
        );
        let done = footer.add_sized(
            done_rect.size(),
            egui::Button::new(RichText::new("Done").size(15.).color(Color32::WHITE))
                .fill(workbench_style::PRIMARY_ACTION)
                .corner_radius(8),
        );
        close.clicked() || done.clicked()
    });
    let escape = modal.is_top_modal
        && !modal.any_popup_open
        && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    (selected, modal.inner || escape)
}

pub(super) fn workbench_dialog_frame(ctx: &egui::Context, margin: f32) -> egui::Frame {
    let visuals = &ctx.style_of(ctx.theme()).visuals;
    egui::Frame::new()
        .inner_margin(margin)
        .fill(visuals.window_fill)
        .corner_radius(12)
        .stroke(visuals.window_stroke)
        .shadow(egui::epaint::Shadow {
            offset: [0, 12],
            blur: 30,
            spread: 0,
            color: Color32::from_black_alpha(86),
        })
}

pub(super) fn workbench_modal(ctx: &egui::Context, id: egui::Id, margin: f32) -> egui::Modal {
    let visuals = &ctx.style_of(ctx.theme()).visuals;
    egui::Modal::new(id)
        .frame(workbench_dialog_frame(ctx, margin))
        .backdrop_color(Color32::from_black_alpha(if visuals.dark_mode {
            153
        } else {
            92
        }))
}

fn child(ui: &mut egui::Ui, id: &'static str, rect: Rect, layout: Layout) -> egui::Ui {
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(id)
            .max_rect(rect)
            .layout(layout),
    );
    child.set_clip_rect(child.clip_rect().intersect(rect));
    child
}

fn page_button(
    ui: &mut egui::Ui,
    height: f32,
    compact: bool,
    active: bool,
    label: &str,
    icon: Icon,
) -> egui::Response {
    let dark = ui.visuals().dark_mode;
    let (fill, accent) = if dark {
        (
            Color32::from_rgb(75, 44, 30),
            Color32::from_rgb(242, 157, 112),
        )
    } else {
        (
            Color32::from_rgb(246, 224, 212),
            Color32::from_rgb(142, 66, 31),
        )
    };
    let glyph = ui.next_auto_id().with("icon");
    let button = egui::Button::new(egui::Atoms::new((
        egui::Atom::custom(glyph, Vec2::splat(23.)),
        RichText::new(label).size(if compact { 14. } else { 16. }),
        egui::Atom::grow(),
    )))
    .selected(active)
    .frame_when_inactive(active)
    .gap(8.)
    .truncate()
    .corner_radius(7);
    let button = if active { button.fill(fill) } else { button };
    let response = button
        .min_size(vec2(ui.available_width(), height))
        .atom_ui(ui);
    let color = if active {
        accent
    } else {
        ui.visuals().weak_text_color()
    };
    if let Some(glyph) = response.rect(glyph) {
        icon.paint(ui, glyph.center(), 22., color);
    }
    if active {
        let x = response.rect.left() + 2.;
        ui.painter().vline(
            x,
            response.rect.center().y - 8. ..=response.rect.center().y + 8.,
            Stroke::new(2., accent),
        );
    }
    response.response
}

impl Icon {
    pub(super) fn paint(self, ui: &egui::Ui, center: egui::Pos2, size: f32, tint: Color32) {
        const SIDE: usize = 48;
        let ctx = ui.ctx();
        let id = egui::Id::new(("mapper-settings-mdi", self));
        let cached = ctx.data(|data| data.get_temp::<egui::TextureHandle>(id));
        let texture = cached.unwrap_or_else(|| {
            macro_rules! asset {
                ($name:literal) => {
                    (
                        $name,
                        include_bytes!(concat!("../assets/settings-icons/", $name, ".alpha")),
                    )
                };
            }
            let (name, mask): (&str, &[u8; SIDE * SIDE]) = match self {
                Self::Globe => asset!("earth"),
                Self::Seed => asset!("sprout-outline"),
                Self::Terrain => asset!("terrain"),
                Self::Biomes => asset!("pine-tree"),
                Self::Monument => asset!("bank-outline"),
                Self::Water => asset!("waves"),
                Self::Road => asset!("road-variant"),
                Self::Rail => asset!("train-variant"),
                Self::Objects => asset!("cube-outline"),
                Self::Pin => asset!("map-marker-outline"),
                Self::Rules => asset!("format-list-checks"),
                Self::Output => asset!("export-variant"),
                Self::Folder => asset!("folder-outline"),
                Self::Performance => asset!("speedometer"),
                Self::Appearance => asset!("theme-light-dark"),
                Self::Viewport => asset!("eye-outline"),
                Self::Updates => asset!("update"),
                Self::Close => asset!("close"),
            };
            let pixels = mask
                .iter()
                .copied()
                .map(Color32::from_white_alpha)
                .collect();
            let texture = ctx.load_texture(
                format!("MDI {name}"),
                egui::ColorImage::new([SIDE; 2], pixels),
                egui::TextureOptions::LINEAR,
            );
            ctx.data_mut(|data| data.insert_temp(id, texture.clone()));
            texture
        });

        ui.painter().image(
            texture.id(),
            Rect::from_center_size(center, Vec2::splat(size)),
            Rect::from_min_max(egui::Pos2::ZERO, pos2(1., 1.)),
            tint,
        );
    }
}
