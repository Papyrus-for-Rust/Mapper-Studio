//! View navigation only: tab selection never changes the generation recipe.
use eframe::egui::{self, Color32, Rect, Stroke, Vec2};
use mapper_studio::native_view::View;

#[derive(Clone, Copy, Default)]
pub(super) struct Availability {
    pub water: bool,
    pub spawns: bool,
    pub painting: bool,
    pub locked: bool,
}
impl Availability {
    fn enabled(self, view: View) -> bool {
        !self.locked
            && (!self.painting || view == View::Topology)
            && (!matches!(view, View::Water | View::ProcessedTopology) || self.water)
            && (view != View::SpawnCandidates || self.spawns)
    }
    fn reason(self, view: View) -> &'static str {
        if self.locked {
            "Finish or cancel the current drag to switch views."
        } else if self.painting && view != View::Topology {
            "Turn off Paint topology to inspect another map view."
        } else if view == View::SpawnCandidates {
            "Generate a composed map with matching spawn metadata first."
        } else {
            "This map has no processed water/topology data yet."
        }
    }
}

// Small line icons share one stroke weight and remain independent of fonts.
fn icon(ui: &egui::Ui, rect: Rect, index: usize, color: Color32) {
    let p = ui.painter();
    let c = rect.center();
    let stroke = Stroke::new(1.5, color);
    let point = |x, y| c + Vec2::new(x, y);
    match index {
        0 => {
            for y in [-5., 0., 5.] {
                p.line_segment([point(-8., y), point(0., y - 4.)], stroke);
                p.line_segment([point(0., y - 4.), point(8., y)], stroke);
                p.line_segment([point(8., y), point(0., y + 4.)], stroke);
                p.line_segment([point(0., y + 4.), point(-8., y)], stroke);
            }
        }
        1 => {
            p.line_segment([point(-9., 7.), point(-2., -7.)], stroke);
            p.line_segment([point(-2., -7.), point(3., 2.)], stroke);
            p.line_segment([point(1., -2.), point(5., -6.)], stroke);
            p.line_segment([point(5., -6.), point(10., 7.)], stroke);
        }
        2 | 3 => {
            for (x, y) in [(-5., -5.), (4., -5.), (-5., 4.), (4., 4.)] {
                if index == 2 {
                    p.circle_stroke(point(x, y), 3., stroke);
                } else {
                    p.rect_stroke(
                        Rect::from_center_size(point(x, y), Vec2::splat(6.)),
                        0.,
                        stroke,
                        egui::StrokeKind::Inside,
                    );
                }
            }
        }
        4 | 6 => {
            for x in [-7., 0., 7.] {
                p.line_segment([point(x, -9.), point(x, 9.)], stroke);
            }
            for y in [-7., 0., 7.] {
                p.line_segment([point(-9., y), point(9., y)], stroke);
            }
            if index == 6 {
                p.circle_filled(point(7., -7.), 3., color);
            }
        }
        5 => {
            for y in [-6., 0., 6.] {
                p.add(egui::Shape::line(
                    vec![
                        point(-9., y),
                        point(-3., y - 2.),
                        point(3., y + 2.),
                        point(9., y),
                    ],
                    stroke,
                ));
            }
        }
        7 => {
            p.circle_stroke(point(0., -5.), 3., stroke);
            p.line_segment([point(0., -2.), point(0., 7.)], stroke);
            p.line_segment([point(-6., 2.), point(6., 2.)], stroke);
        }
        _ => {
            p.rect_stroke(
                Rect::from_center_size(c, Vec2::new(16., 12.)),
                2.,
                stroke,
                egui::StrokeKind::Inside,
            );
            p.line_segment([point(-5., -3.), point(5., -3.)], stroke);
            p.line_segment([point(-5., 2.), point(5., 2.)], stroke);
        }
    }
}

pub(super) fn reserve(ui: &mut egui::Ui, selected: &mut View, state: Availability) -> bool {
    if ui.available_width() < 100. {
        return false;
    }
    egui::Panel::right(ui.id().with("map-view-tabs"))
        .exact_size(46.)
        .resizable(false)
        .show_separator_line(false)
        .frame(egui::Frame::NONE.inner_margin(egui::Margin {
            left: 6,
            ..egui::Margin::ZERO
        }))
        .show(ui, |ui| tabs(ui, selected, state))
        .inner
}

fn tabs(ui: &mut egui::Ui, selected: &mut View, state: Availability) -> bool {
    let mut changed = false;
    egui::ScrollArea::vertical()
        .id_salt("map-view-scroll")
        .show(ui, |ui| {
            for (index, view) in View::ALL.into_iter().enumerate() {
                let enabled = state.enabled(view);
                let response = ui.add_enabled(
                    enabled,
                    egui::Button::new("")
                        .min_size(Vec2::splat(34.))
                        .selected(*selected == view),
                );
                let color = ui.style().interact(&response).fg_stroke.color;
                icon(ui, response.rect, index, color);
                if response.clicked() && *selected != view {
                    *selected = view;
                    changed = true;
                }
                response
                    .on_hover_text(view.label())
                    .on_disabled_hover_text(format!("{}\n{}", view.label(), state.reason(view)));
            }
            ui.separator();
            for name in [
                "Train tunnels",
                "Underwater lab floors",
                "Dungeons",
                "Deep Sea",
            ] {
                let response =
                    ui.add_enabled(false, egui::Button::new("").min_size(Vec2::splat(34.)));
                icon(ui, response.rect, 8, ui.visuals().weak_text_color());
                response.on_disabled_hover_text(format!(
                    "{name}\nInterior map data and rendering are not supported yet."
                ));
            }
        });
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Pos2;
    #[test]
    fn view_availability_preserves_painting_and_drag_guards() {
        let full = Availability {
            water: true,
            spawns: true,
            ..Default::default()
        };
        assert!(View::ALL.into_iter().all(|v| full.enabled(v)));
        let missing = Availability::default();
        assert!(!missing.enabled(View::Water));
        assert!(!missing.enabled(View::ProcessedTopology));
        assert!(!missing.enabled(View::SpawnCandidates));
        let painting = Availability {
            painting: true,
            ..full
        };
        for view in View::ALL {
            assert_eq!(painting.enabled(view), view == View::Topology);
        }
        assert!(
            !Availability {
                locked: true,
                ..full
            }
            .enabled(View::Overview)
        );
    }
    #[test]
    fn right_tabs_switch_views_keep_canvas_separate_and_render_both_themes() {
        mod snapshot {
            include!("../examples/support/egui_snapshot.rs");
        }
        for dark in [true, false] {
            for width in [430, 1000] {
                let ctx = egui::Context::default();
                ctx.set_visuals(if dark {
                    egui::Visuals::dark()
                } else {
                    egui::Visuals::light()
                });
                let mut selected = View::Overview;
                let mut textures = snapshot::Textures::default();
                let mut rail_x = 0.;
                let mut first_y = 0.;
                let mut canvas = Rect::NOTHING;
                let out = {
                    let mut draw = |selected: &mut View, events: Vec<egui::Event>, locked| {
                        let mut out = ctx.run_ui(
                            egui::RawInput {
                                screen_rect: Some(Rect::from_min_size(
                                    Pos2::ZERO,
                                    Vec2::new(width as f32, 650.),
                                )),
                                events,
                                focused: true,
                                ..Default::default()
                            },
                            |ui| {
                                let available = ui.available_rect_before_wrap();
                                rail_x = available.right() - 23.;
                                first_y = available.top() + 17.;
                                reserve(
                                    ui,
                                    selected,
                                    Availability {
                                        locked,
                                        ..Default::default()
                                    },
                                );
                                canvas = ui.available_rect_before_wrap();
                                ui.painter()
                                    .rect_filled(canvas, 0., super::super::MAP_BACKGROUND);
                            },
                        );
                        textures.update(&out.textures_delta);
                        out.textures_delta.clear();
                        out
                    };
                    draw(&mut selected, vec![], false);
                    // Heights is the second 34pt tab, separated by the native item spacing.
                    let x = width as f32 - 31.;
                    let y = 8.
                        + 34.
                        + ctx
                            .style_of(if dark {
                                egui::Theme::Dark
                            } else {
                                egui::Theme::Light
                            })
                            .spacing
                            .item_spacing
                            .y
                        + 17.;
                    let button = |pressed| egui::Event::PointerButton {
                        pos: Pos2::new(x, y),
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    };
                    draw(
                        &mut selected,
                        vec![egui::Event::PointerMoved(Pos2::new(x, y)), button(true)],
                        false,
                    );
                    draw(&mut selected, vec![button(false)], false);
                    let out = draw(&mut selected, vec![], false);
                    assert_eq!(selected, View::Heights);
                    let surface_y = 25.;
                    let surface = |pressed| egui::Event::PointerButton {
                        pos: Pos2::new(x, surface_y),
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    };
                    draw(
                        &mut selected,
                        vec![
                            egui::Event::PointerMoved(Pos2::new(x, surface_y)),
                            surface(true),
                        ],
                        true,
                    );
                    draw(&mut selected, vec![surface(false)], true);
                    assert_eq!(selected, View::Heights);
                    out
                };
                assert!(canvas.right() < rail_x - 17.);
                assert!(first_y > 0.);
                if let Some(dir) = std::env::var_os("MAPPER_UI_SNAPSHOT_DIR") {
                    let dir = std::path::PathBuf::from(dir);
                    std::fs::create_dir_all(&dir).unwrap();
                    let image = textures.raster(&ctx, out, [width, 650]);
                    mapper_studio::image_export::write_png(
                        &dir.join(format!(
                            "map-view-tabs-{}-{width}.png",
                            if dark { "dark" } else { "light" }
                        )),
                        &image,
                        || false,
                    )
                    .unwrap();
                }
            }
        }
    }
}
